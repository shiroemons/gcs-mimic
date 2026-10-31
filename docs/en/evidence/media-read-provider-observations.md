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
| A compressed response adds `vary: Accept-Encoding` | Appended after the CORS `vary: Origin`; omitted on gunzipped and `no-transform` responses | the same tests |

`X-Origin` is not sent locally. XML expansion previously returned 501, and JSON
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
other than one `gzip` fail 400. The provider's 400 on a later identity GET of a
decompressed object that still declares `contentEncoding: gzip` is not
reproduced; gcs-mimic aborts that download like any other invalid stored gzip.

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
opaque strong ETag on every representation. HEAD with `Range` on a stored-gzip
object sent compressed was not observed; locally it states the range length.
