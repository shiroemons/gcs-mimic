# Response wire format observed on Cloud Storage

Observed **2026-10-31** with curl requests sent to `storage.googleapis.com`
from an opt-in test project, using a private, uniquely named bucket with
uniform bucket-level access in `asia-northeast1` that was deleted afterwards.
The project, project number, bucket and principals are not recorded; the
bucket appears as `<bucket>` below. Only per-request infrastructure headers
(`date`, `server`, `alt-svc`, `x-guploader-uploadid`) were removed from the
recording. The same requests were replayed against gcs-mimic.

Objects: `plain.txt`, the 16 bytes `plain text body\n` (MD5
`3S6ENAmCNQC429h/U5OY5Q==`), and `gz.txt`, a gzip member. Gzip transcoding,
JSON media HEAD, the `contentEncoding` upload parameter and `Vary` headers
are recorded in [media read observations](media-read-provider-observations.md)
and are not part of this document.

## Entity tags

| Response | Recorded value |
| --- | --- |
| JSON object metadata and media `ETag` header | `CK+S+qOG5JcDEAE=`, unquoted; equal to the resource `etag` |
| JSON bucket `ETag` header and `etag` | `CAE=` |
| XML object GET/HEAD `ETag` | `"dd2e843409823500b8dbd87f539398e5"` (quoted MD5 hex) |
| XML list `<ETag>` | `"dd2e843409823500b8dbd87f539398e5"`, with literal quote characters |

`CK+S+qOG5JcDEAE=` decodes to `08 af92faa386e49703 10 01`: protobuf field 1
is the generation `1793442589870383` as a varint and field 2 the
metageneration `1`. `CAE=` decodes to `08 01`, the metageneration alone. The
same encoding was already used for ACL entry etags.

### gcs-mimic behavior

| Observation | Local behavior | Test |
| --- | --- | --- |
| JSON ETag encoding | Object and bucket tags use the recorded encoding in header and body; ACL entries, objects, buckets and gRPC messages share one encoder; XML keeps the quoted MD5 and quotes the JSON tag for composite and encrypted objects (that XML form is unrecorded) | `wire_parity_http::json_etags_encode_generation_and_metageneration_like_cloud_storage`, `json::object_tests::obj_render_001_exact_resource_and_encoded_generation_links`, `json::tests::bkt_id_002_etag_encodes_only_the_metageneration` |
| SDKs echo the unquoted tag | A bare or quoted `If-Match`/`If-None-Match` member names the same JSON tag; members with whitespace or stray quotes, and a wildcard inside a list, still return 400 | `http_conditions::tests::unquoted_json_validators_match_bare_and_quoted_members`, `etag::json_etag_read_syntax_and_missing_resources`, `metadata_if_match_reuses_strong_lists_wildcards_and_complete_syntax_validation` |
| Recreated bucket | The tag encodes only the metageneration, so a recreated bucket at the same metageneration repeats it. This is inferred from the encoding; a repeat was not observed | `etag::json_etag_read_buckets_and_metadata_changes_in_both_profiles` |

## Cache headers

| Response | `cache-control` | `pragma` |
| --- | --- | --- |
| JSON object metadata, object list, bucket metadata | `private, max-age=0, must-revalidate, no-transform` | absent |
| JSON media 200 and 206 | `no-cache, no-store, max-age=0, must-revalidate` | `no-cache` |
| JSON `objects.insert` (`uploadType=media`) | `no-cache, no-store, max-age=0, must-revalidate` | `no-cache` |
| JSON error (404 metadata) | `no-cache, no-store, max-age=0, must-revalidate` | `no-cache` |
| JSON media 404 and 416 | `private, max-age=0` | absent |
| XML private object GET/HEAD (including 206), XML list, XML 404 | `private, max-age=0` | absent |

Only `uploadType=media` inserts were recorded as writes. Patch, update, copy,
compose, rewrite, move, restore, delete, bucket writes and resumable or
multipart uploads were not observed.

### gcs-mimic behavior

| Observation | Local behavior | Test |
| --- | --- | --- |
| Metadata and list reads | Bucket and object get and list send the resource value without `pragma`; IAM policy reads keep `no-cache` because they were not recorded | `wire_parity_http::cache_headers_follow_each_recorded_response_kind`, `etag::json_etag_read_object_metadata_media_and_range` |
| Media 200 and 206 | `cache-control` is the no-store value; public objects keep their stored or `public, max-age=3600` value. The media `pragma` belongs to the media-header slice | `wire_parity_http::cache_headers_follow_each_recorded_response_kind`, `json::object_tests::obj_render_002_media_headers_match_selected_generation_and_hashes` |
| Insert and JSON errors | No-store value plus `pragma: no-cache` on object inserts (media and multipart, the same method) and on every JSON error body; a 304 stays bodyless | `wire_parity_http::object_inserts_and_json_write_errors_forbid_every_cache` |
| Media 404 and 416 | `private, max-age=0` without `pragma`; other media errors keep the JSON error headers | `wire_parity_http::cache_headers_follow_each_recorded_response_kind` |
| XML | Lists and error documents send `private, max-age=0`; private objects already did and a stored directive still wins | `wire_parity_http::cache_headers_follow_each_recorded_response_kind`, `xml_cache_control_respects_metadata_and_distinguishes_public_from_authenticated` |

## Error bodies

| Request | Recorded response |
| --- | --- |
| JSON metadata GET of a missing object | `application/json; charset=UTF-8`; fields `code`, `message`, `errors[{message, domain, reason}]`; message `No such object: <bucket>/missing.txt`, reason `notFound` |
| JSON media GET of a missing object | `text/html; charset=UTF-8`, body `No such object: <bucket>/missing.txt` |
| JSON media HEAD with an unsatisfiable range | 416, `text/html; charset=UTF-8`, `content-range: bytes */<size>`, `cache-control: private, max-age=0` (HEAD, so no body was seen) |
| XML GET of a missing object | `<?xml version='1.0' encoding='UTF-8'?><Error><Code>NoSuchKey</Code><Message>The specified key does not exist.</Message><Details>No such object: <bucket>/missing.txt</Details></Error>` |

A follow-up probe recorded how object names are written into these bodies: the
JSON media 404 HTML-escapes the name (`&` to `&amp;`, `<` to `&lt;`, `>` to
`&gt;`, `"` to `&quot;`, `'` to `&#39;`) and sends no `x-content-type-options`
or content-security-policy header, while the XML `Details` escapes only `&`,
`<` and `>` and leaves quotes literal.

### gcs-mimic behavior

| Observation | Local behavior | Test |
| --- | --- | --- |
| Missing object messages | JSON get, patch and delete of a missing object say `No such object: <bucket>/<object>`; copy, compose and rewrite sources keep the generic `Object not found` because the request path names a different object | `wire_parity_http::missing_object_errors_name_the_object_like_cloud_storage` |
| Media 404 | `text/html; charset=UTF-8` with the escaped message and `private, max-age=0`; the name is escaped because the response is HTML and the name is client-controlled | `wire_parity_http::missing_object_errors_name_the_object_like_cloud_storage`, `wire_parity_http::missing_object_names_are_escaped_for_the_document_they_appear_in` |
| Media 416 | `text/html; charset=UTF-8`, `content-range: bytes */<size>` and `private, max-age=0`; the GET body text (the local range message) was not recorded | `wire_parity_http::unsatisfiable_media_ranges_answer_with_html_text` |
| XML NoSuchKey | Fixed `Message` plus a `Details` element naming the object, escaped as XML character data | `wire_parity_http::missing_object_names_are_escaped_for_the_document_they_appear_in` |

Remaining differences are tracked as `SPEC-RESPONSE-WIRE-REMAINING` in
`compatibility/gaps.json`.
