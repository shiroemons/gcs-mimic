# Media read headers observed on Cloud Storage

Observed **2026-10-31** with curl requests sent to `storage.googleapis.com`
from an opt-in test project, using private, uniquely named buckets that were
deleted afterwards. The project, buckets and principals are not recorded.
Only the response headers that gcs-mimic now reproduces are summarized here.

## Range reads and `x-goog-hash`

Object: the 10 bytes `0123456789`, CRC32C `KAwGng==`,
MD5 `eB5eJF1ptWaXm4bijSPyxw==`.

| Request | Status | `x-goog-hash` |
| --- | --- | --- |
| JSON `alt=media`, no Range | 200 | `crc32c=KAwGng==,md5=eB5eJF1ptWaXm4bijSPyxw==` (one field) |
| JSON `Range: bytes=2-5` | 206, `bytes 2-5/10` | absent |
| JSON `Range: bytes=7-` | 206, `bytes 7-9/10` | absent |
| JSON `Range: bytes=-3` | 206, `bytes 7-9/10` | absent |
| JSON `Range: bytes=0-9` (whole object) | 206, `bytes 0-9/10` | `crc32c=KAwGng==` only |
| XML GET, no Range | 200 | two fields: `crc32c=KAwGng==` and `md5=eB5eJF1ptWaXm4bijSPyxw==` |
| XML `Range: bytes=2-5` | 206 | the same two whole-object fields |

The JSON 206 responses keep `x-goog-stored-content-length: 10` and
`x-goog-stored-content-encoding: identity`.

gcs-mimic previously sent the whole-object CRC32C and MD5 on every JSON 206.
Clients that validate any received hash against the downloaded bytes, such as
the official .NET SDK, then failed range reads with a hash mismatch.

### gcs-mimic behavior

| Observation | Local behavior | Test |
| --- | --- | --- |
| JSON partial range omits `x-goog-hash` | Omitted | `obj_range_http_single_ranges_and_representation_headers` |
| JSON whole-object range sends only CRC32C | `crc32c=...` only, including clamped and oversized suffix ranges that cover the object | `obj_range_http_single_ranges_and_representation_headers` |
| JSON 200 sends one combined field | Unchanged | `obj_range_http_single_ranges_and_representation_headers` |
| XML GET/HEAD and XML 206 send one field per algorithm | `crc32c=` and, when the object has an MD5, `md5=` as separate fields | `xml::xml_head_matches_get_headers_and_selected_metadata`, `xml::xml_ranges_generation_and_preconditions_use_one_live_generation` |

XML upload, copy, compose and POST responses still send the combined field;
they were not part of this observation.

## Decompressive transcoding

Object: a 35-byte gzip member stored with `Content-Encoding: gzip`.

| Request | Status | Notable headers |
| --- | --- | --- |
| JSON, no `Accept-Encoding` or `Accept-Encoding: identity` | 200, expanded body | `x-guploader-response-body-transformations: gunzipped`, `warning: 214 UploadServer gunzipped`, stored-object `x-goog-hash`, no Content-Length or Content-Encoding, `vary: Origin` and `vary: X-Origin` |
| JSON, `Accept-Encoding: gzip` | 200, stored bytes | `content-encoding: gzip`, `content-length: 35`, `vary: Origin`, `vary: X-Origin`, `vary: Accept-Encoding` |
| JSON identity with `Range: bytes=0-4` | 200, whole expanded body | Range ignored while transcoding |
| XML, `Accept-Encoding: identity` | 200, expanded body | the same two transformation fields and two `x-goog-hash` fields |

### gcs-mimic behavior

| Observation | Local behavior | Test |
| --- | --- | --- |
| Gunzipped responses carry the two transformation fields | Sent on JSON media GET and XML GET/HEAD | `json_gzip_negotiation_range_and_conditions_preserve_stored_representation`, `xml_gzip_gunzips_whole_object_and_keeps_stored_hashes` |
| Range is ignored while transcoding | Full 200 on JSON and XML | the same tests |
| XML expansion keeps one hash field per algorithm | Unchanged stored-object hashes | `xml_gzip_gunzips_whole_object_and_keeps_stored_hashes` |
| A compressed response adds `vary: Accept-Encoding` | Appended after the CORS `vary: Origin` (and, on JSON, `vary: X-Origin`); on JSON omitted on gunzipped and `no-transform` responses, on XML sent on every stored-gzip representation (see below) | the same tests |

XML expansion previously returned 501, and JSON
expansion did not mark the transformed body.

## Uploads that declare `contentEncoding`

Observed **2026-10-31**. Object: a 49-byte gzip member of a 29-byte text,
uploaded with `uploadType=media&contentEncoding=gzip` and no request
`Content-Encoding` header.

| Request | Cloud Storage | gcs-mimic before |
| --- | --- | --- |
| `uploadType=media&contentEncoding=gzip` | 200; `contentEncoding: gzip`, `size: 49`, the stored MD5 and CRC32C of the gzip bytes | 400, the parameter was rejected |

### gcs-mimic behavior

| Observation | Local behavior | Test |
| --- | --- | --- |
| The query parameter sets the stored `contentEncoding` | Accepted on media, multipart and resumable uploads; the stored bytes are unchanged | `json_media_upload_content_encoding_query_sets_stored_encoding`, `multipart_content_encoding_query_matches_resumable_declaration_rules` |

Local choices that were not observed: an invalid value (for example one
containing a control character) fails 400 before the body is read, and a
multipart or resumable metadata body that declares a different
`contentEncoding` fails 400, as resumable initiation already did.

## Media uploads sent with `Content-Encoding: gzip`

Observed **2026-10-31** in a separate, discarded run. A media upload with the
`contentEncoding=gzip` query parameter that also carried the request header
`Content-Encoding: gzip` and a gzip body was stored **decompressed**: its
`size` was that of the plain text, while its metadata still said
`contentEncoding: gzip`. A later identity GET of that object failed with
HTTP 400 and an 11-byte `text/plain` body while still sending
`x-guploader-response-body-transformations: gunzipped`.

### gcs-mimic behavior

| Observation | Local behavior | Test |
| --- | --- | --- |
| The request `Content-Encoding: gzip` is removed before storing | Media uploads stream through the bounded upload gzip decoder that multipart uploads already use; the decoded bytes are stored and the query `contentEncoding` still sets the metadata | `media_transport_gzip_stores_the_decoded_body` |

Local choices that were not observed: non-gzip, truncated or CRC-corrupt
bodies fail 400, a decoded body above `max_object_bytes` fails 413, and codings
other than one `gzip` fail 400. The later read is covered in the next section.

## JSON media HEAD

Observed **2026-10-31** on the 49-byte gzip object above and a 16-byte
identity object, on `/storage/v1/b/{bucket}/o/{object}?alt=media` and
`/download/storage/v1/b/{bucket}/o/{object}?alt=media`.

| Request | Status | Notable headers |
| --- | --- | --- |
| gzip object, no `Accept-Encoding` or `identity`, either endpoint | 200, no body | the GET headers: weak `etag: W/...`, both transformation fields, stored-object `x-goog-hash`, no Content-Length |
| gzip object, `Accept-Encoding: gzip` | 200, no body | strong ETag, `content-encoding: gzip`, `vary: Origin`, `vary: X-Origin`, `vary: Accept-Encoding`, no Content-Length (the matching GET sends `content-length: 49`) |
| identity object | 200, no body | the GET headers including `content-length: 16` |

gcs-mimic previously returned 400 on `/storage/v1` and 405 on `/download`.

### gcs-mimic behavior

| Observation | Local behavior | Test |
| --- | --- | --- |
| HEAD sends the media GET headers without a body | Both endpoints; the object body is not read | `json_media_head_sends_get_headers_without_a_body` |
| Only the identity HEAD sends Content-Length | Removed for stored-gzip HEAD, gunzipped or compressed | the same test |

The weak ETag on gunzipped responses is not reproduced: gcs-mimic keeps its
opaque strong ETag on every representation.

## `Vary` on JSON API responses

Observed **2026-10-31**. Object metadata GET, media GET and HEAD, object
list and a 404 object GET each sent `vary: Origin` and `vary: X-Origin` as two
fields, with or without an `Origin` request header. A compressed stored-gzip
media response added `vary: Accept-Encoding` as a third field. gcs-mimic sent
only `vary: Origin`.

### gcs-mimic behavior

| Observation | Local behavior | Test |
| --- | --- | --- |
| `vary: Origin` and `vary: X-Origin` on every JSON response | Sent as two fields by the JSON CORS wrapper on every routed JSON response, including errors | `json_responses_vary_by_origin_and_x_origin` |
| `vary: Accept-Encoding` follows them on a compressed response | Appended after the two fields | the same test |

JSON preflight responses keep their single
`Origin, Access-Control-Request-Method, Access-Control-Request-Headers` field;
preflight was not part of this observation. Errors returned before routing,
such as a rejected request `Content-Encoding`, carry no `Vary` field.

## `Vary` and length on XML API responses

Observed **2026-10-31** on a bucket without CORS configuration, for the
49-byte gzip object and the 16-byte identity object.

| Request | Cloud Storage | gcs-mimic before |
| --- | --- | --- |
| identity object GET/HEAD, with or without `Origin` | no `Vary` field; `accept-ranges: bytes`, `content-length: 16` | `vary: Origin` |
| gzip object GET/HEAD, gunzipped (no `Accept-Encoding` or `identity`) | `vary: Accept-Encoding`, no `accept-ranges`, no Content-Length | `vary: Origin` only |
| gzip object GET, `Accept-Encoding: gzip` | `vary: Accept-Encoding`, `accept-ranges: bytes`, `content-length: 49` | `vary: Origin` and `vary: Accept-Encoding` |
| gzip object HEAD, `Accept-Encoding: gzip` | `vary: Accept-Encoding`, `accept-ranges: bytes`, no Content-Length | `vary: Origin`, `vary: Accept-Encoding` and `content-length: 49` |

### gcs-mimic behavior

| Observation | Local behavior | Test |
| --- | --- | --- |
| No `vary: Origin` on a bucket without CORS configuration | Omitted unless the bucket has CORS rules or the `cors_fallback` profile is active | `xml_vary_follows_bucket_cors_and_stored_gzip`, `signed_rules_change_between_preflight_and_actual_and_survive_restart` |
| `vary: Accept-Encoding` on every stored-gzip GET/HEAD | Sent on gunzipped and compressed responses; omitted with `no-transform` | `xml_vary_follows_bucket_cors_and_stored_gzip`, `xml_gzip_gunzips_whole_object_and_keeps_stored_hashes` |
| Gunzipped responses omit `accept-ranges`; compressed keep it | Unchanged from the gunzip support above | the same tests |
| A compressed HEAD omits Content-Length | Removed on stored-gzip HEAD | `xml_vary_follows_bucket_cors_and_stored_gzip` |

**Unverified:** a bucket *with* CORS configuration was not observed. gcs-mimic
keeps `vary: Origin` there, also when the request has no `Origin` or matches no
rule, because the response then depends on the Origin. Preflight on a bucket
without CORS configuration now also omits `Vary`; that was not observed either.
XML requests now read the bucket's CORS rules even without an `Origin` header,
so a catalog failure during that read returns the XML error before routing.

## Gunzipping a plain body

Observed **2026-10-31**. A 29-byte plain text uploaded with
`Content-Encoding: gzip` and the `contentEncoding=gzip` query was stored
decompressed (the upload answered 200). Reads of that object:

| Request | Status | Notable headers |
| --- | --- | --- |
| JSON `alt=media`, no `Accept-Encoding` or `identity` | 400, `text/plain; charset=utf-8`, body `Bad Request` (11 bytes) | the gunzipped headers: both transformation fields, stored-object `x-goog-hash`, `x-goog-stored-content-encoding: gzip`, `x-goog-stored-content-length: 29`, weak ETag, `content-length: 11` |
| XML GET, `Accept-Encoding: identity` | 400, the same body | the same transformation fields and one `x-goog-hash` field per algorithm |
| JSON, `Accept-Encoding: gzip` | 200, the 29 plain bytes | `content-encoding: gzip`, `content-length: 29` |

### gcs-mimic behavior

| Observation | Local behavior | Test |
| --- | --- | --- |
| A gunzipping read of non-gzip bytes fails before any header | JSON media GET and XML GET check the first stored bytes against the gzip magic and answer 400 with the 11-byte body and the gunzipped headers; only one bounded chunk is read | `gunzipping_a_plain_body_fails_400_before_the_headers_are_sent` |
| `Accept-Encoding: gzip` returns the stored bytes with 200 | Unchanged | the same test |

Local limits: only the leading magic bytes are checked, so a body that starts
like gzip and breaks later still aborts mid-stream (as before). An empty
object is not rejected. HEAD reads no bytes and is not checked; the provider's
HEAD for such an object was not observed. The weak ETag is not reproduced here.

## HEAD with `Range`

Observed **2026-10-31** on the 29-byte identity object and the 49-byte
stored-gzip object.

| Request | Status | Notable headers |
| --- | --- | --- |
| JSON media HEAD, `Range: bytes=0-4`, identity | 206, no body | `content-range: bytes 0-4/29`, `content-length: 5`, no `x-goog-hash` |
| XML HEAD, `Range: bytes=0-4`, identity | 206, no body | `content-range: bytes 0-4/29`, `content-length: 5`, both `x-goog-hash` fields |
| JSON media HEAD, `Accept-Encoding: gzip`, `Range: bytes=0-4`, stored gzip | 206, no body | `content-range: bytes 0-4/49`, `content-encoding: gzip`, no Content-Length |
| XML HEAD, `Accept-Encoding: gzip`, `Range: bytes=0-4`, stored gzip | 206, no body | `content-range: bytes 0-4/49`, `content-encoding: gzip`, no Content-Length, both `x-goog-hash` fields |
| JSON media HEAD, `Range: bytes=100-200` | 416 | `content-range: bytes */29`, `content-type: text/html; charset=UTF-8` |

### gcs-mimic behavior

| Observation | Local behavior | Test |
| --- | --- | --- |
| HEAD with a satisfiable Range is 206 on both APIs | JSON and XML answer 206 with `content-range`; the identity range length is `content-length`; a compressed stored-gzip HEAD states none | `head_with_range_answers_206_and_content_range_on_json_and_xml` |
| HEAD with an unsatisfiable Range is 416 | JSON and XML answer 416 with `content-range: bytes */size` and no body | the same test |

XML HEAD used to answer 200 for a Range, following the method reference. The
JSON media HEAD no longer seeks the object. The XML unsatisfiable HEAD was not
observed and follows the JSON observation; the provider's 416 body type
(`text/html`) is not reproduced.
