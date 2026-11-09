# JSON HTTP batch requests

## Contract

Each server admits two batch envelopes, including response bodies being sent.
Admission precedes body reads and stays held until the response body is consumed
or dropped on disconnect. A client that stops reading therefore keeps its place. Additional batches wait without reading their bodies;
on HTTP connections the earlier of request_timeout_ms (default
180000 ms) and object_request_timeout_ms (batch admission, default 0, disabled)
ends the wait with JSON 503 backendError. Expiry of the request
deadline also closes the connection with `Connection: close`. An admitted envelope's body
then runs under body_inactivity_timeout_ms, like every request body, and its silence answers
408 uploadBrokenConnection. The request stays below
10,000,000 bytes, as the batch guide documents. The response has no total limit: it leaves
chunked, one part at a time, so memory follows the largest part rather than their sum; this is
not a bound on whole-process RSS. Outer authentication
still supplies defaults that an individual part may override.

POST /batch/storage/v1 accepts a multipart/mixed envelope containing between
one and 100 application/http parts. Content-ID is optional; a supplied ID is
returned with the response- prefix, inside angle brackets when the input used
them. Responses follow input order. Execution is sequential locally, but callers
must not depend on ordering: the Google service can execute calls in any order.

Each inner request uses an HTTP/1.1 request line with a relative /storage/v1/
target. Raw encoded object paths reach the existing JSON dispatcher unchanged.
An absolute target is accepted only on the advertised API endpoint
(api_base_url): its scheme and authority must equal the advertised origin after
the public-origin normalization (case-insensitive scheme and DNS host, default
port omitted), and its path must continue below the advertised path prefix with
/storage/v1/. The Java client sends such request lines, and Cloud Storage answers
them like relative paths. Any other scheme, host, port, userinfo or prefix
rejects the whole envelope with 400 before any part runs, so a part is never
forwarded to another server. Scheme-relative targets, Host, hop-by-hop headers,
nested batches, helper and internal endpoints, upload routes and alt=media
downloads are rejected. The
outer envelope's Content- headers and transport headers are not inherited.
Other outer headers and query parameters supply defaults; an inner value for
the same header or decoded query key replaces the default. Every operation
authenticates and authorizes independently through the normal handlers.

All MIME structure, request targets, part framing and sizes are validated before
the first operation executes. A malformed later part therefore cannot cause an
earlier deletion. After successful envelope validation, an individual operation's
404, 403, 412 or other error becomes that part's HTTP response while other parts
continue. Batch execution is not an atomic catalog transaction.

The dispatcher is called directly. No socket, DNS lookup or second connection
slot is needed, including with max_connections=1. Existing condition checks,
retention/hold checks, strict permissions and catalog publication remain in use.

## Bounds and local limits

The request must be smaller than 10,000,000 bytes. Each MIME/HTTP header block is
limited to 8 KiB and 64 headers; an inner body has no limit of its own below the
envelope's. Preamble and epilogue each allow 4 KiB. Boundaries use the existing RFC 2046 validator,
including quoted values and trailing delimiter whitespace. Header folding,
non-binary transfer encoding, duplicate framing headers, incorrect Content-Length,
HTTP trailers, compressed batch envelopes and non-HTTP/1.1 parts are rejected.

Line endings follow the Cloud Storage observation below rather than RFC 2046's CRLF-only
grammar. LF is accepted wherever CRLF is: on delimiter lines, part MIME headers, inner
request lines and inner headers. A delimiter counts only at the start of a line. The
opening delimiter line's ending selects how later delimiters are found, so in a CRLF
envelope an LF-only `--boundary` line is body content, and in an LF envelope a CR before
the delimiter's LF stays in the body. Only the delimiter's own line ending is removed from
a body, so a CRLF envelope that closes with an LF-only delimiter never closes and answers
400. An inner header block may omit its blank line where the part ends, including directly
after the request line; such a part has an empty body, and a declared Content-Length must
still match it. The MIME block keeps its required blank line. The rules for mixed line
endings in one envelope are local behavior: Cloud Storage was observed on the four forms
below only.

One bounded blocking parser job runs at a time. Validated request bodies retain
shared slices of the bounded envelope, rather than duplicate object bodies.
Each part's response leaves as soon as it is ready. A part may buffer up to 1 GiB, above the
largest response that documented limits allow, so a limit a real response could reach never
reports a completed operation as a failure; past it the part is replaced by a per-part 413
responseTooLarge error that states that the operation may have completed.

The response head leaves before any part runs. A client that stops reading holds back the
remaining parts, and one that disconnects cancels them. Each part starts its own deadline under
request_timeout_ms, and an expired part answers its own 503 in place while later parts still run;
progress and catalog waits renew that deadline as they do for a request. A producer that stops
early fails the body instead of ending it without its closing delimiter. These response limits and
exact malformed-input statuses are local behavior, without real-service parity claims.

The broad feature BEHAVIOR-COMMON-008 remains partial until final combined SDK
verification and the remaining provider/profile edge contract are reconciled.
This HTTP envelope is separate from the Storage Batch Operations service.

## Sources

The batch guide checked on 2026-10-31 says request lines "must only contain the
path portion of the URL; full URLs can have undefined behavior", and that a
request that cannot be parsed into sub-requests returns 400. A Cloud Storage
observation on 2026-10-31 answered absolute parts on its own endpoint normally
([evidence](../evidence/http-batch-absolute-targets.md)); foreign origins were
not observed, so the local 400 for them is a safety choice.

A Cloud Storage observation on 2026-11-09 answered a DELETE part with outer 200 and inner
`HTTP/1.1 204 No Content`, and removed the object, in four framings: CRLF with a blank line
before the delimiter's CRLF; CRLF with the blank line sharing the delimiter's CRLF, as
OpenDAL sends it; LF throughout; and the request line followed directly by the delimiter.

Checked on 2026-10-30:

- [Google batch contract](https://docs.cloud.google.com/storage/docs/batch),
  retrieved HTML SHA256
  3038d4a8af36898a901e39af0f988c272dc92580808764c985788f1f51f6dce7.
- [Reference server source](https://github.com/fsouza/fake-gcs-server/blob/896ece51e49eed0946089f3089f6343855c0d106/fakestorage/server.go),
  fake-gcs-server v1.56.1 revision
  896ece51e49eed0946089f3089f6343855c0d106, SHA256
  abe448cd575fbc5687a2527031c3b72fc1ff4b109b12333d1b1d031ab533c005.
  The reference's permissive buffering and missing-Content-ID handling do not
  define the Google contract or local resource/security limits.

## Acceptance and verification

tests/it_protocol/http_batch.rs covers metadata PATCH and atomic stale conditions, mixed
200/204/404/412 parts, optional and matching Content-IDs, encoded object names,
strict outer credentials with denied/invalid inner overrides, inner query
overrides, rejected inherited/inner query credentials, one connection slot,
100/101 calls, the 10 MB boundary, MIME framing,
invalid targets/headers/lengths and full-envelope validation before deletion.
`batch_answers_full_object_lists_past_the_former_response_limits` covers full object lists, and
`server::batch::tests` cover parts leaving as their own chunks, a stalled reader, a departed
reader and a part past its limit. The observed line-ending forms, CRLF and LF request bodies,
delimiter detection only at line starts, retained trailing body bytes and the header, preamble,
epilogue and call limits under LF framing are covered by the
`batch_deletes_under_each_recorded_line_ending_form`,
`batch_passes_part_bodies_intact_in_both_line_endings`,
`batch_recognizes_delimiters_only_at_line_starts`,
`batch_keeps_body_bytes_up_to_the_delimiter_line_ending` and
`batch_line_ending_tolerance_keeps_limits_and_rejections` wire tests;
`server::batch::parse::tests` assert the body bytes each part hands its handler.

On 2026-10-30, cargo check --tests passed and cargo test --test http_batch
passed all four wire tests. SDK, container and full integration execution are
reserved for the final combined source. No throughput or RSS claim is made.
The server::batch::tests case passed for bounded per-part response errors,
Content-ID and byte-correct response Content-Length. The production lint gate
reported zero violations.

The final shared HTTP regression command was
cargo test --test http_batch --test http_ingress --test http_multipart_gzip
--test object_multipart_http: 36 passed, zero failed. The two existing
server::upload_frame_tests and both gzip producer/budget unit tests passed.
Owned Rust formatting, both slice documents' Markdown lint and git diff --check
passed. No dependency, ledger or generated-matrix files were changed by this slice.
