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
