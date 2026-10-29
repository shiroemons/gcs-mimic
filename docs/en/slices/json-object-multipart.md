# JSON object multipart upload

<!-- markdownlint-configure-file {"MD013": {"tables": false}} -->

Contract checked: **2026-10-22**. Scope: BEHAVIOR-OBJECT-009 and multipart
acceptance additions to JSON-OBJECTS-007, BEHAVIOR-OBJECT-001,
BEHAVIOR-OBJECT-005 and BEHAVIOR-COMMON-011. This contract decomposes the
supported subset; `compatibility/features.json` remains the status authority.
`objects.insert` remains PARTIAL because resumable uploads are separate P3 work.
Multipart also remains PARTIAL with LOCAL_SIMULATION fidelity: the scoped local
cases pass, while SPEC-013/SPEC-016, ACL/IAM permissions and direct-service edge
evidence remain incomplete. The policy requires no deviations and matching target
fidelity before IMPLEMENTED can be claimed.

## Acceptance cases

| Behavior | Expected result |
| --- | --- |
| POST uploadType=multipart | 200 complete modeled Object resource after validated publication |
| Two-part multipart/related | JSON metadata first, binary media second; empty media is valid |
| Optional third checksum part | application/json with md5Hash and/or crc32c; validate before publication |
| Invalid boundary, malformed/truncated body or wrong part count | 400; no object creation or replacement; accept two or three parts only |
| Boundary across transport chunks | Preserve exact media bytes, including binary, delimiter-like bytes and trailing CRLF |
| Preamble/epilogue and part headers | Bounded parsing; no object-size-proportional buffering |
| Metadata/checksum JSON cap | At most 64 KiB per JSON part; oversized part returns 413 |
| Writable metadata | Persist name, fixed content fields and custom metadata; apply the existing metadata validation rules |
| Custom metadata size | UTF-8 key/value total <=8192 bytes; excess returns 400 without publication |
| Query name versus metadata name | Query wins; missing both returns 400; validate the selected name |
| Path bucket versus metadata bucket | Path wins; string metadata bucket accepted without changing the target; non-string returns 400 |
| Content type | Metadata contentType, then media-part Content-Type, then application/octet-stream; local precedence |
| Metadata/third-part checksums | Correct MD5/CRC32C succeeds; malformed or mismatched values return 400 without publication |
| Conflicting checksum sources | 400; equal declarations allowed; third-part unknown keys return 400; local choices |
| Generation/metageneration conditions | All four existing conditions enforced atomically; mutation mismatch returns 412 |
| Concurrent create-if-absent | Exactly one successful generation; other request returns 412 |
| Failed overwrite | Previous bytes, metadata and generation remain visible |
| Streaming media size cap | Apply max_object_bytes to media bytes, excluding multipart framing; excess returns 413 |
| Admission and advisory checks | Immediate bounded admission; bucket check before metadata; selected-name conditions before media |
| Transfer/catalog deadlines | Reuse media inactivity, absolute request and catalog-wait limits |
| Persistence | Successful bytes, metadata, hashes and generation survive restart |
| Recognized unsupported/unknown metadata | Explicit 501; no accepted-but-ignored semantics except documented bucket override |
| Resumable upload | Explicit 501; no session or object published |
| Pinned Go SDK | ChunkSize=0 Writer metadata/checksum round trip, DoesNotExist, conditioned overwrite, Attrs and NewReader |

The [integration evidence](../evidence/multipart.md) records final commands and
results.
Exact wire and parser test names are listed below.

| Coverage | Test names |
| --- | --- |
| Boundary syntax | `json::multipart::tests::multipart_boundary_accepts_quoted_rfc_values_and_rejects_invalid_parameters` |
| Chunking and bounded carry | `json::multipart::tests::multipart_parser_preserves_boundary_prefixes_at_every_chunk_split`, `json::multipart::tests::multipart_parser_streams_large_media_with_bounded_carry`, `obj_multipart_http_streams_one_byte_chunks_and_boundary_like_binary` |
| Framing and exact caps | `json::multipart::tests::multipart_parser_reads_long_headers_and_padding_in_one_byte_frames`, `json::multipart::tests::multipart_parser_bounds_preamble_epilogue_headers_and_metadata`, `json::multipart::tests::multipart_parser_rejects_missing_parts_bad_headers_and_truncation`, `obj_multipart_http_rejects_malformed_framing_and_bounds_final_json` |
| Metadata and restart | `obj_multipart_http_round_trip_metadata_hashes_and_restart`, `obj_multipart_http_rejects_bad_metadata_and_unsupported_fields_without_mutation`, `obj_multipart_http_query_name_overrides_only_valid_metadata_names` |
| Content type and custom limits | `obj_multipart_http_null_and_empty_content_type_fall_back_to_media_part`, `obj_multipart_http_custom_metadata_cap_rejects_before_media_or_staging`, `obj_multipart_http_media_json_and_custom_metadata_caps` |
| Checksums and failed overwrite | `obj_multipart_http_checks_metadata_header_and_final_part_hashes_before_publication`, `obj_multipart_http_malformed_checksum_json_preserves_existing_generation`, `obj_multipart_http_truncated_checksum_part_preserves_existing_generation` |
| Preconditions and tokens | `json::object_tests::obj_headers_upload_tokens_require_one_nonempty_value_with_or_without_conditions`, `obj_http_upload_tokens_with_generation_match_allow_one_create_and_overwrite`, `obj_http_upload_tokens_reject_empty_or_duplicate_headers_before_body`, `obj_multipart_http_preconditions_reject_before_body_and_preserve_previous_generation`, `obj_multipart_http_upload_tokens_with_generation_match_allow_one_create_and_overwrite`, `obj_multipart_http_upload_tokens_reject_empty_or_duplicate_headers_before_body` |
| Concurrency and deadlines | `obj_multipart_http_concurrent_create_if_absent_has_one_winner_and_no_leaks`, `obj_multipart_http_deadline_cancels_admitted_bodies_and_releases_all_slots` |
| Official Go Writer | `TestObjectMultipartWriter`; `TestObjectMediaLifecycle` retains the resumable rejection |

## Supported subset

`POST /upload/storage/v1/b/{bucket}/o?uploadType=multipart` accepts
`Content-Type: multipart/related; boundary=...`. Quoted boundaries and MIME boundary
syntax follow RFC 2046 section 5.1.1. Multipart framing is removed before streaming
media into the existing opaque blob store. JSON metadata comes first, media second,
and an optional checksum JSON object third. One part or four or more parts fails.
A missing closing delimiter, malformed framing or premature EOF must never publish
the staged object. HTTP transport chunk boundaries have no MIME significance.

The [upload guide](https://docs.cloud.google.com/storage/docs/uploading-objects#json-api-multipart-upload)
shows metadata then media and returns 200 with metadata. The
[data-validation guide](https://docs.cloud.google.com/storage/docs/data-validation#json-api-multipart-uploads)
also documents a third JSON checksum part. The latter is part of this slice's
acceptance scope, rather than treating every third part as malformed.

Metadata supports `name`, `contentType`, `contentEncoding`, `contentDisposition`,
`contentLanguage`, `cacheControl`, `metadata`, `md5Hash` and `crc32c`. Fixed fields
and custom keys/values reuse the [metadata slice](json-object-metadata.md) validation:
Unicode custom metadata and empty values survive; control characters or wrong types
fail 400; empty custom keys and non-ASCII fixed header values fail 501 locally.
Content-Language retains the 100-character limit. The custom map is capped at
8192 UTF-8 key/value bytes for non-null entries; null entries are absent from the
new map. Excess is rejected after bounded metadata parsing, before media input
or staging, with a second check at publication. The JSON part has the 64 KiB cap.
Uploads create a fresh metadata set rather than merging with a previous generation.

The [insert reference](https://docs.cloud.google.com/storage/docs/json_api/v1/objects/insert)
and Discovery agree that query `name` overrides metadata `name`, and the path
bucket overrides metadata `bucket`. A string metadata bucket is accepted and
ignored; a non-string returns 400. Without a query name, metadata must provide
the name. Metadata `name` must always be a string or null, even when query `name`
overrides it; invalid types return 400 `invalidArgument`. Null or missing name
without a query name returns 400 `required`. Existing once-only URL decoding and
object-name validation still apply. `kind` is accepted only as `storage#object`;
other server-managed inputs conservatively return 501.

Stored content type uses metadata `contentType` first, then the media part's
`Content-Type`, then `application/octet-stream`. Null or empty metadata contentType
is absent and uses this fallback. Google's resource reference
defines the last value as the serving default, but does not establish this whole
upload precedence or acceptance of a missing media-part header. These are local
choices pending direct service evidence. The outer multipart Content-Type is
framing and must never become the stored object content type.

`md5Hash` and `crc32c` contain standard base64, with 16-byte MD5 and four-byte
big-endian CRC32C. Validate all supplied values against incrementally computed
hashes of the exact media bytes. Checksum mismatch returns 400, as documented by
the data-validation guide. Hash syntax and exact error reasons remain local rules
under SPEC-010. The optional third part is `application/json`, bounded to 64 KiB,
and may contain only these two checksum fields. An empty object is accepted.
Conflicting metadata/header/third-part declarations and other third-part keys return
400 locally; identical declarations are allowed. Publication waits for validation
of the entire multipart container, including this part, final delimiter, bounded
epilogue and transport EOF. The official third-part example shows CRC32C;
accepting MD5 there, an empty object, equal declarations, conflict rejection and
unknown-key rejection are local choices under SPEC-016.

All four `ifGenerationMatch`, `ifGenerationNotMatch`, `ifMetagenerationMatch` and
`ifMetagenerationNotMatch` conditions reuse the media upload parser and atomic
publication transaction. Zero generation-match provides create-if-absent.
Unconditional upload replaces the live object. Successful new bytes receive a
distinct generation and metageneration 1; failed requests leave the prior record
untouched. Concurrent create-if-absent uploads must have one winner. Generation
history and read-style 304 behavior are not added to mutation responses.

Media and multipart uploads accept one nonempty `X-Goog-Gcs-Idempotency-Token`
with or without generation conditions. Matching successful retries return the
original resource through the bounded persistent [upload replay contract](sdk-upload-replay.md),
including conditioned retries. Conflicting requests fail without publication;
empty or duplicate token headers return 400 `invalidParameter`. SPEC-013 remains
open for other methods and exact provider retention/error semantics.
Shared empty/duplicate token validation also reaches objects.list and PATCH;
dedicated malformed-token HTTP tests for those two routes are not present.

Return the complete modeled Object JSON with fixed/custom metadata, computed
checksums, decimal-string size/generation/metageneration, timestamps and existing
opaque identifiers/links. This means the existing supported resource subset,
not synthesized ACL, owner, retention or encryption effects. `projection=full`
and `noAcl` retain the existing modeled projection boundary. Persist multipart
results with the existing catalog/blob publication and restart recovery protocol;
there is no new schema or independent multipart storage backend.

## Bounds, errors and unsupported semantics

The existing object cap applies to media bytes, not JSON or MIME overhead.
Unknown-length and chunked bodies remain bounded streams. Metadata, part headers,
preamble, epilogue and retained boundary fragments have fixed limits independent
of object size. Each JSON part accepts exactly 65536 bytes. Preamble and epilogue
each accept 4096 bytes; the preamble excludes the CRLF belonging to the first
delimiter. Each part's 8192-byte header-block cap includes every line's CRLF and
the terminating blank line. Delimiter suffix lines accept at most 8192 bytes
before CRLF, including the closing `--` when present; CRLF split across frames
at the exact cap is accepted. Known Content-Length above the media cap plus a
conservative 256 KiB framing allowance returns 413 before upload admission.
These are local resource limits under SPEC-016; RFC 2046 does not prescribe them.

Reuse immediate upload admission and the media deadlines. Bucket existence is
checked before metadata parsing. A query-name condition check also precedes body
input; a metadata-only name receives its advisory check after bounded metadata
and before media consumption. Publication rechecks bucket
and conditions atomically. This staged advisory order is a local extension of
SPEC-008, needed because the final name may exist only in the first part.
Body inactivity, the absolute object-request ceiling and bounded catalog waits
retain their existing settings and timeout outcomes. A post-EOF response timeout
can leave the eventual mutation outcome unknown; re-read or retry with conditions.
No transaction spans media transfer. Functional bounded-stream tests do not
establish RSS/cgroup/CPU benchmarks or power-loss durability.

The local error map is:

| HTTP / reason | Cases |
| --- | --- |
| 400 `invalidParameter` | Invalid MIME/type/boundary/headers, malformed framing or truncation, non-object checksum JSON, checksum syntax/conflicts, empty/duplicate token |
| 400 `parseError` | Malformed metadata or third-part checksum JSON |
| 400 `invalidArgument` | Metadata field/name/bucket types, invalid fixed/custom metadata, custom UTF-8 key/value sum above 8192 bytes |
| 400 `invalid` | Actual MD5/CRC32C mismatch or unknown third-part checksum keys |
| 400 `required` | No query name and missing/null metadata name |
| 413 `uploadTooLarge` | Media cap, either JSON-part cap or conservative known-length cap exceeded |
| 501 `notImplemented` | Unsupported metadata/output/query/header semantics |

Existing missing-bucket
404, mutation precondition 412, excess-admission/catalog-unavailable 503 and
unsupported-semantic 501 boundaries remain. Failure before validated publication
must not expose partial bytes or metadata. Error reason strings are local until
independently verified; SPEC-016 retains the multipart container questions.

Transport errors and trailers encountered inside a multipart body map to 400
`invalidParameter`, unlike the media path's `badRequest` body-abort error. A
headers-only body part lacking the additional CRLF expected by this parser is
rejected. Folded/duplicate part headers and Content-Transfer-Encoding other than
`binary` (including `7bit` and `8bit`) return 400 `invalidParameter`. Delimiter
spaces/tabs are supported. These restrictions are local compatibility limits,
not independently verified Google errors. No CPU/RSS performance claim is made.

Recognized advanced metadata (ACL/IAM, holds, retention, contexts, customTime,
storage class and encryption), other server-managed fields and unknown metadata
fields return 501. The path-bucket override is the explicitly supported exception.
This does not assert Google's unknown/output-only input handling; SPEC-015 remains
open. Unsupported query/header semantics retain the existing explicit rejections.
SDK invocation tokens follow the bounded replay rule above. Resumable sessions
remain 501.
XML multipart uploads, nested/alternate-root MIME composition, strict credentials,
IAM permission checks, decompressive transcoding and historical generations are
outside this slice. Google requires object-create permission and additionally
object-delete permission for overwrites; local success establishes neither.

RFC 2387 defines generic related-root parameters, including `type` and `start`.
The Google upload example and pinned SDK emit boundary-only related containers
with metadata first. That application format is the compatibility target; no
general MIME document renderer or alternate-root selection is claimed. Outer
parameters accept `boundary` and `type=application/json`; other semantics return
501. Metadata/checksum JSON part headers may specify UTF-8 charset.

## Sources and SDK provenance

All URLs checked on 2026-10-22. The live Storage JSON v1 Discovery fetched again
on this date retains revision `20261013`, SHA-256
`1f74b7a93b3d2872cc332fc20dafae1657fc255930ce336618bea3d0b4d95ae0`.
Its simple upload path is `/upload/storage/v1/b/{bucket}/o` with `multipart=true`;
the separate resumable declaration does not establish local resumable support.

- [Uploading objects and JSON multipart example](https://docs.cloud.google.com/storage/docs/uploading-objects)
- [Multipart how-to, redirected to uploading objects](https://docs.cloud.google.com/storage/docs/json_api/v1/how-tos/multipart-upload)
- [Object uploads and transfer-mode distinctions](https://docs.cloud.google.com/storage/docs/uploads)
- [objects.insert](https://docs.cloud.google.com/storage/docs/json_api/v1/objects/insert)
- [Object resource and checksum encodings](https://docs.cloud.google.com/storage/docs/json_api/v1/objects)
- [Data validation and third-part checksums](https://docs.cloud.google.com/storage/docs/data-validation)
- [Object and custom-metadata limits](https://docs.cloud.google.com/storage/quotas#objects)
- [Request preconditions](https://docs.cloud.google.com/storage/docs/request-preconditions)
- [RFC 2046, section 5.1.1](https://www.rfc-editor.org/rfc/rfc2046#section-5.1.1)
- [RFC 2387, related content and root parameters](https://www.rfc-editor.org/rfc/rfc2387)
- [Live Storage v1 Discovery](https://storage.googleapis.com/$discovery/rest?version=v1)
- [Retained methods](../../../compatibility/upstream/json-methods.json)
- [Retained schemas](../../../compatibility/upstream/json-schemas.json)
- [Storage SDK v1.69.0 Writer](https://github.com/googleapis/google-cloud-go/blob/storage/v1.69.0/storage/writer.go)
- [Storage SDK v1.69.0 HTTP writer](https://github.com/googleapis/google-cloud-go/blob/storage/v1.69.0/storage/http_client.go)
- [API v0.307.0 upload selection and MIME assembly](https://github.com/googleapis/google-api-go-client/blob/v0.307.0/internal/gensupport/media.go)

The pinned harness uses Go 1.27.3, Storage v1.69.0 and API v0.307.0 without a
dependency change. Storage's HTTP writer passes `ChunkSize` to API media options.
`PrepareUpload` returns `singleChunk=true` for zero chunk size and `UploadType`
returns `multipart`; `CombineBodyMedia` assembles metadata and media. Writer
serializes explicit MD5/CRC32C into metadata, so its coverage exercises two-part
requests. Third-part validation belongs to wire-contract tests. To retain the
resumable negative case, the harness sends more than one 256 KiB chunk with a
nonzero chunk size. JSON reads are selected explicitly, and the shared emulator
transport rejects requests outside the configured local origin.
