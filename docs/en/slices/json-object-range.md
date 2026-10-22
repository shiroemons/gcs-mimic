# JSON object byte-range reads

Contract checked on **2026-10-22**. This slice extends JSON media downloads on
`/storage/v1/b/{bucket}/o/{object}?alt=media` and
`/download/storage/v1/b/{bucket}/o/{object}?alt=media`.
Implementation commit: `f1e258096225adf98167ad02abb89b1a722961bd`.
[Verification](../evidence/object-range.md) records executed checks;
[the feature ledger](../../../compatibility/features.json) remains the status authority.
This contract supersedes the earlier media slice's Range rejection and absence of
`Accept-Ranges`; the other media limitations still apply.

## Official sources

All links below were checked on 2026-10-22. Google JSON API v1 and HTTP/1.1 are
the implemented protocols; RFC 9110 (June 2022) supplies HTTP range semantics.

| Source | Contract used |
| --- | --- |
| [objects.get](https://docs.cloud.google.com/storage/docs/json_api/v1/objects/get) | `alt=media`, Range support, generation selection, generation/metageneration conditions and `storage.objects.get` permission |
| [JSON request headers](https://docs.cloud.google.com/storage/docs/json_api/v1/parameters#range) | Closed, open-ended and suffix ranges; contiguous response; multiple ranges are not accepted; Content-Range and stored-content headers |
| [XML request headers](https://docs.cloud.google.com/storage/docs/xml-api/reference-headers#range) | Explicit 206 for successful single ranges; corresponding generation/metageneration/hash header definitions |
| [JSON status codes](https://docs.cloud.google.com/storage/docs/json_api/v1/status-codes#416_requested_range_not_satisfiable) | 416 with reason `requestedRangeNotSatisfiable` |
| [Streaming reads](https://docs.cloud.google.com/storage/docs/streaming-reads#json-api) | Pin the generation across successive ranged reads; the old streaming-downloads URL redirects here |
| [Sliced downloads](https://docs.cloud.google.com/storage/docs/sliced-object-downloads) | Parallel range GETs assemble one object; this slice does not add a parallel download scheduler |
| [Transcoding](https://docs.cloud.google.com/storage/docs/transcoding#using_the_range_header) | Decompression ignores Range; `Accept-Encoding: gzip` or stored `no-transform` prevents decompression |
| [Checksum validation](https://docs.cloud.google.com/storage/docs/data-validation#performing_data_integrity_checks_on_objects_that_were_downloaded_or_read) | Server checksums cover the complete stored object; they cannot validate an isolated JSON range |
| [RFC 9110](https://www.rfc-editor.org/rfc/rfc9110.html#section-14) | Inclusive endpoints, clamping, large numerals, 416 length indication, permitted ignore behavior and empty-representation suffix rules |

Discovery was fetched again from
[`storage.v1`](https://storage.googleapis.com/$discovery/rest?version=v1):
revision `20261013`, SHA-256
`1f74b7a93b3d2872cc332fc20dafae1657fc255930ce336618bea3d0b4d95ae0`.
Its get method advertises media downloads and the existing selectors/conditions;
Discovery does not specify Range error bodies or an exhaustive response header set.
No dependency was added. Verification uses Rust 1.99.1, Go 1.27.3,
Storage v1.69.0 and API client v0.307.0 from the committed lockfiles.

## Supported subset and decisions

For a nonempty selected representation, `bytes=first-last`, `bytes=first-`, and
`bytes=-suffix` return 206. Endpoints are inclusive; excessive final offsets clamp
to the last byte and an oversized suffix selects the complete representation as
a 206 response. Decimal numerals do not overflow. Units are case-insensitive and
leading zeroes are accepted.

An offset at or beyond the stored length, or a zero suffix, returns 416 with
`Content-Range: bytes */size`. Closed/open ranges on empty objects return 416.
The body uses the existing JSON envelope, numeric `error.code=416`, global domain
and `requestedRangeNotSatisfiable` reason. The local message is
`The requested Range cannot be satisfied`; exact production wording is unverified.
`Content-Type` is `application/json; charset=UTF-8`, Content-Length describes
that JSON body, and successful representation/hash headers are absent.
Google documents the status/reason; the length header follows RFC 9110 §15.5.17.

RFC 9110 §14.1.2 treats a nonzero suffix as satisfiable even when the representation
is empty. For that case this emulator ignores Range and returns 200 with length
zero and no Content-Range, as §14.2 permits. A blanket claim that every empty-object
range must return 416 would be incorrect; Google's precise empty-suffix response
has not been independently verified.

Malformed syntax, unknown units, comma-separated ranges and repeated Range fields
are ignored, yielding a complete 200 response without Content-Range. Google rules
out multiple partial ranges but does not document their exact failure/fallback
response. Ignoring them is an explicit local HTTP rule, not a claim of matching
every undocumented Google edge response. Metadata GETs retain their existing
behavior and do not apply Range to JSON resources.

If-Range remains 501 `notImplemented`, including ETag and date forms. The checked
Google references do not establish this JSON validator subset; RFC definitions
alone do not establish Google's support. ETag conditional requests remain outside
this slice, with their existing explicit rejection.

### Headers on 206

All values come from the same selected record as the opened blob.

| Header | Emitted value | Evidence boundary |
| --- | --- | --- |
| `Content-Range` | `bytes first-last/size` | JSON header reference and RFC range contract |
| `Content-Length` | Number of selected bytes | JSON Content-Length definition |
| `Accept-Ranges` | `bytes`, also on successful complete media responses | RFC advertisement; exact Google presence is unverified |
| `Content-Type` | Stored type, or `application/octet-stream` | Existing media behavior; partial parity tested locally |
| `Content-Disposition`, `Content-Language` | Stored values when present | Retained representation metadata; complete Google partial header set is unverified |
| `Cache-Control` | `no-cache` | Existing JSON nonpublic-object rule; stored public cache metadata does not grant public access |
| `ETag` | Existing quoted opaque local ETag for the selected generation/metageneration | JSON reference documents the field; exact provider encoding remains SPEC-012 |
| `x-goog-generation`, `x-goog-metageneration` | Selected generation/metageneration in decimal | XML definitions, local wire assertions and pinned Go reader generation assertion; exhaustive JSON parity remains SPEC-012 |
| `x-goog-hash` | Full stored-object CRC32C and MD5, unchanged from complete media responses | Hash semantics documented by the validation guide; exact partial-header presence remains unverified |
| `x-goog-stored-content-length` | Complete stored length, not range length | JSON stored-content definition; SDK reader reports full size |
| `x-goog-stored-content-encoding` | Stored encoding, or `identity` | JSON stored-content definition |
| `Content-Encoding` | Stored encoding when present | No transcoding in this slice |

No range checksum is invented. Applications must not compare the range bytes'
checksum to `x-goog-hash`. Headers whose presence is unverified stay part of the
existing JSON header-parity gap, even though their local values are tested.

### Gzip, generations, authorization and resources

Ranges address compressed bytes when the stored encoding is gzip and the request
accepts gzip, or stored Cache-Control includes `no-transform`. Content-Encoding
remains gzip. A request requiring decompressive transcoding still returns the
existing 501; it does not return compressed bytes as false success. Implementing
Google's ignored-Range, full decompressed response requires a separate slice.

The catalog authorizes `storage.objects.get`, selects the requested live generation,
evaluates existing preconditions and opens its immutable blob under the same
execution permit. Range evaluation follows those checks. Anonymous strict requests
return 401 and insufficient project roles return 403 before range handling;
failed match conditions remain 412 and failed GET not-match conditions remain
empty 304. Combined-condition precedence remains the existing SPEC-008 rule.

Seek operates on that already-open handle, outside the catalog transaction.
The reader emits at most `min(remaining, 65536)` bytes per chunk, stops at the
requested length and reports unexpected EOF as an error. It never reads the skipped
prefix. Existing one-chunk response-channel backpressure and request deadlines
remain in place. The sparse-file test checks the cursor before the first read,
chunk lengths, exact stopping position, a two-byte read and premature EOF.
The HTTP overwrite test receives 206 headers before replacing an 8 MiB object,
then verifies every remaining response byte belongs to the original generation.
Historical versions are not retained: a fresh request for an overwritten generation
returns 404, while an already-open reader survives its blob's unlink.

## Acceptance and exact test IDs

All cases below passed on 2026-10-22. IDs describe behavior and are suitable as
row-local acceptance IDs in the ledger proposal.

| ID | Acceptance | Result | Exact test IDs |
| --- | --- | --- | --- |
| CLOSED | Both media prefixes return exact inclusive bytes, 206, bounds and lengths; first/last byte and clamped end work | PASS | `obj_range_http_single_ranges_and_representation_headers`; `TestObjectRangeReader/closed`; `TestObjectRangeReader/clamped` |
| OPEN-ENDED | Offset through EOF returns exactly the remaining bytes | PASS | `obj_range_http_single_ranges_and_representation_headers`; `TestObjectRangeReader/open_ended` |
| SUFFIX | Suffix and oversized suffix return correct bytes; oversized numerals are bounded safely | PASS | `obj_range_http_single_ranges_and_representation_headers`; `TestObjectRangeReader/suffix`; `TestObjectRangeReader/large_suffix` |
| UNSATISFIABLE | At/end-past offsets and zero suffix return 416, correct JSON reason and `bytes */size` | PASS | `obj_range_http_unsatisfiable_empty_and_ignored_ranges`; `TestObjectRangeReader` |
| EMPTY | Empty closed/open ranges fail 416; nonzero empty suffix uses explicit 200 fallback | PASS | `obj_range_http_unsatisfiable_empty_and_ignored_ranges` |
| FALLBACK | Invalid/non-bytes/multiple/repeated ranges return complete 200; If-Range forms fail 501 | PASS | `obj_range_http_unsatisfiable_empty_and_ignored_ranges`; `obj_headers_003_range_is_accepted_and_unsupported_conditionals_are_rejected` |
| HEADERS | Partial representation headers retain selected identity and full stored hashes/length; complete response has no Content-Range | PASS | `obj_range_http_single_ranges_and_representation_headers`; `obj_render_002_media_headers_match_selected_generation_and_hashes` |
| GENERATION | Explicit generation, missing/stale generation, all four preconditions and overwrite during an open response | PASS | `obj_range_http_generation_preconditions_and_overwrite_pin_open_reader`; `TestObjectRangeReader` |
| AUTHORIZATION | Anonymous/creator denied before satisfiable, unsatisfiable and If-Range evaluation; viewer reads suffix | PASS | `obj_range_http_strict_authorization_precedes_range_evaluation` |
| GZIP | Gzip acceptance/no-transform selects stored bytes; required transcoding fails explicitly | PASS | `obj_range_http_gzip_ranges_use_stored_bytes_without_transcoding` |
| BOUNDED-READ | Seek near the end of a 64 MiB sparse file; <=64 KiB chunks, exact length and EOF failure | PASS | `storage::catalog::objects::tests::object_range_reader_seeks_sparse_blob_and_bounds_every_chunk` |

The full workspace test run also retains the existing negative upload/conditional
tests; the previous valid-Range=501 assertion now checks 206 and exact bytes.

## Getting-started addition

After the existing guide creates `example-bucket` and uploads a nonempty
`hello.txt`, the integration guide can add:

```sh
curl --fail-with-body -i -H 'Range: bytes=0-4' \
  'http://127.0.0.1:4443/download/storage/v1/b/example-bucket/o/hello.txt?alt=media'
```

For an object containing `hello world`, expect 206, body `hello`,
`Content-Range: bytes 0-4/11` and `Content-Length: 5`.
Use the uploaded generation as an additional `generation` query parameter when
assembling multiple ranges. Strict mode also needs the existing viewer/admin
local bearer-token header. This snippet is proposed guide text; the HTTP and SDK
tests verify its behavior, but this exact curl invocation was not separately run.

## Deviations and limitations

Proposed open gap `SPEC-RANGE-FALLBACK`: establish Google's malformed, repeated,
multi-range and empty-suffix responses with authoritative clarification or
explicitly authorized differential tests. Current 200 fallbacks are local rules.
The generic JSON 416 reason is documented; exact message, empty-object edge
selection and exhaustive error headers are not independently verified.

Proposed open gap `SPEC-RANGE-VALIDATORS`: establish the JSON If-Range ETag/date
subset before implementation; local 501 remains visible. Extend SPEC-012 to
track the partial response header presence and opaque ETag questions above.
Retain SPEC-015 for unavailable transcoding and SPEC-009 for the ordinary JSON
endpoint's direct response instead of production redirect behavior.

No XML/gRPC adapter, multipart/byteranges response, version history, public IAM/ACL
policy, decompression or parallel downloader is added. No real Google bucket,
metadata server, container build or container integration test was used.

## Ledger proposal

Shared ledgers and generated views are intentionally unchanged in this worktree.
The integration owner should copy the acceptance/test mappings above and link
verification to [the final check log](../evidence/object-range-checks.txt) and
[Go log](../evidence/object-range-go.txt), dated 2026-10-22.

- **BEHAVIOR-OBJECT-016:** propose `IMPLEMENTED`, target/actual fidelity `EXACT`
  for documented single-range, untranscoded reads only. Adopt `CLOSED`,
  `OPEN-ENDED`, `SUFFIX`, `UNSATISFIABLE`, `GENERATION`, `AUTHORIZATION` and
  `BOUNDED-READ`, plus the Content-Range/Content-Length checks from `HEADERS`.
  Keep fallback, empty-suffix and exhaustive header cases on JSON-OBJECTS-005.
  Proposed notes: "JSON media single byte ranges support
  inclusive/open/suffix reads, clamping, 206 Content-Range, 416 unsatisfiable
  ranges, generation pinning, existing preconditions and strict authorization;
  bounded seek-based streaming. Scope and fallback boundaries:
  docs/en/slices/json-object-range.md; evidence:
  docs/en/evidence/object-range.md. Malformed/multi/non-bytes ranges follow a
  documented local RFC 9110 rule; If-Range returns 501; these, empty-object
  suffix selection and complete header parity remain open on JSON-OBJECTS-005
  and its range gaps."
- **JSON-OBJECTS-005:** keep `PARTIAL`. Add the same range acceptance/test IDs;
  add notes: "Single untranscoded JSON byte ranges now work; If-Range and
  transcoding remain unsupported; malformed/multiple/empty-suffix fallback and
  partial-header parity are explicit source questions." Replace the previous
  blanket Range/If-Range rejection note with these supported/unsupported subsets.
  Keep SPEC-008/009/012/015 and the proposed range gaps visible.
- **DESIGN-082:** keep `PARTIAL`. Add acceptance ID `GO-RANGE`, description
  "Pinned official Go JSON NewRangeReader handles closed/open/suffix/clamped,
  at-end/past-end and generation-conditioned reads", result `PASS`, exact test
  ID `TestObjectRangeReader`. SDK evidence: Go `1.27.3`, Storage `v1.69.0`,
  API client `v0.307.0`, transport `HTTP JSON; storage.WithJSONReads`, command
  `GCS_MIMIC_GO_TEST_RUN=TestObjectRangeReader bash scripts/sdk-go.sh`, result
  `PASS`, artifact `docs/en/evidence/object-range-go.txt`. A later SDK pin change
  needs its own verification; this evidence must retain its actual version.

Suggested next slice: resumable JSON uploads. If exact edge-response fidelity
is prioritized first, resolve the range source gaps before widening this scope.
