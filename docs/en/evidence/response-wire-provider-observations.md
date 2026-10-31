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

Remaining differences are tracked as `SPEC-RESPONSE-WIRE-REMAINING` in
`compatibility/gaps.json`.
