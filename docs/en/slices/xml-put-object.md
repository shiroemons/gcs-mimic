# Bounded path-style XML PUT Object

<!-- markdownlint-configure-file {"MD013": {"tables": false}} -->

The subsequent [signed-upload contract](signed-upload.md) adds V4 RSA query
authentication and bounded optional SHA256 validation. References below to
signing/content SHA256 as unsupported describe this original slice.

Contract checked: **2026-10-23**. Feature: `EXT-XML-022`. The feature ledger is
the implementation-status source of truth; the full official method remains
PARTIAL because this slice deliberately rejects advanced inputs.

## Contract sources

- [PUT Object](https://docs.cloud.google.com/storage/docs/xml-api/put-object-upload)
- [XML headers](https://docs.cloud.google.com/storage/docs/xml-api/reference-headers)
- [XML status](https://docs.cloud.google.com/storage/docs/xml-api/reference-status)
- [XML permissions](https://docs.cloud.google.com/storage/docs/access-control/iam-permissions)
- [Request preconditions](https://docs.cloud.google.com/storage/docs/request-preconditions)

The existing Discovery and Storage v2 source pins in `docs/en/sources.md` describe
the shared metadata model; XML behavior comes from the protocol-specific pages.
Source digests and executed checks are recorded in the evidence document.

## Frozen acceptance cases

| Case | Required behavior |
| --- | --- |
| Framing and bytes | `PUT /bucket/object`, raw path split before one decoding pass, explicit Content-Length including zero or chunked framing; default served content type application/octet-stream; exact empty/small/multiple-buffer bodies |
| Fixed metadata | Preserve Content-Type, Cache-Control, Content-Disposition, Content-Encoding and Content-Language through XML reads and JSON metadata GET; overwrite resets omitted metadata |
| Custom metadata | Lowercase HTTP key suffixes; ASCII-representable values including empty values; combined stored key/value bytes at most 8192; reject repeated case-folded keys and empty suffixes before body reads |
| MD5 | Exactly one canonical base64 Content-MD5 value, decoded directly; prefixed values, appended hash lists and noncanonical padding fail 400 InvalidDigest; x-goog-hash md5 is parsed separately, conflicting declarations fail 400 InvalidDigest and mismatched bytes fail 400 BadDigest |
| CRC32C | Big-endian base64 x-goog-hash crc32c, alone or combined/repeated hash headers with MD5; malformed declaration fails 400 InvalidDigest and mismatch fails 400 BadDigest under the local checksum-error rule |
| Conditions | Single decimal generation match, including zero create-if-absent, and positive metageneration match; advisory check before body, authoritative check atomically with publication; exactly one competing conditional PUT wins |
| Authorization | Strict local tokens require create and additionally delete on live overwrite; project and bucket permissions are additive; allUsers may grant anonymous writes; invalid tokens never become anonymous; permissive uses the same storage transitions without auth checks |
| Unsupported input | Unknown query and x-goog inputs, ACL/predefined ACL, CSEK/CMEK, class override, requester billing, retention/hold, resumable, content SHA256/signing, copy, ranges, HTTP validators, trailers and AWS extension semantics fail 501 before body reads |
| Errors and preservation | XML Error/Code/Message; 400 InvalidAuthentication, 401 AuthenticationRequired, 403 AccessDenied, authorized 404 NoSuchBucket, 412 PreconditionFailed, 411 MissingContentLength, configured-size 400 EntityTooLarge; failed requests preserve previous bytes, metadata, history and generation counter |
| Success | 200 empty body, quoted lowercase hexadecimal MD5 ETag, generation, metageneration, whole-object x-goog-hash, stored length and stored encoding (identity when absent) |
| Resource bounds | Reuse JSON's two-slot 64 KiB upload bridge, eight active-upload admission limit, transfer/request/catalog deadlines, shared incremental checksums, opaque disk staging and recovery protocol; exact max succeeds and max+1 fails for known and streamed sizes |
| CORS | Bucket XML PUT preflight and actual-response rules apply to success and errors; CORS grants no object permission; JSON rules remain unchanged |
| Persistence and interactions | Restart preserves PUT data/metadata; incomplete disconnect/shutdown cannot publish; restart removes abandoned staging/orphans; versioning retains replaced generations; bucket default event hold applies and prevents overwrite |

## Local boundaries

`SPEC-XML-PUT-EDGES` records duplicate/custom/hash-error choices and parser
boundaries not established by the official pages. Hyper owns malformed HTTP
framing and Expect: 100-continue: accepted requests receive 100 when body polling
starts, while early failures return a final error without requesting the body.
Transport-invalid framing may fail before the XML adapter can format an error.
Any syntactically valid nonempty PUT query returns 501 before interpreting a
generation selector, including a malformed numeric value. Malformed query
encoding or duplicate keys retain the shared XML query parser's 400 response.

Single-request uploads share the existing commit/recovery protocol. Before EOF,
disconnect, timeout or shutdown aborts publication. After confirmed EOF,
filesystem/SQLite publication may finish even if the response wait expires or
the client disconnects; the outcome is then unknown, as documented for JSON.
This slice does not promise cancellation of an already-running commit or power
loss durability. `SPEC-XML-PUT-EDGES` retains that boundary explicitly.

Signed PUT verification is intentionally separate from this slice. Signature
requests are not accepted as anonymous strict writes. Unsupported storage class
overrides fail even when equal to the bucket default. Retry tokens require a
generation-match condition and do not replay earlier responses.

Go storage v1.70.0 writes through JSON rather than XML PUT. Rust raw HTTP wire
tests therefore exercise this XML operation directly; another Go HTTP client
would repeat the same contract without adding official SDK coverage.
