# JSON HTTP batch requests

## Contract

POST /batch/storage/v1 accepts a multipart/mixed envelope containing between
one and 100 application/http parts. Content-ID is optional; a supplied ID is
returned with the response- prefix, inside angle brackets when the input used
them. Responses follow input order. Execution is sequential locally, but callers
must not depend on ordering: the Google service can execute calls in any order.

Each inner request uses an HTTP/1.1 request line with a relative /storage/v1/
target. Raw encoded object paths reach the existing JSON dispatcher unchanged.
Absolute URLs, authorities, Host, hop-by-hop headers, nested batches, helper and
internal endpoints, upload routes and alt=media downloads are rejected. The
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
limited to 8 KiB and 64 headers; each inner body is limited to 64 KiB. Preamble
and epilogue each allow 4 KiB. Boundaries use the existing RFC 2046 validator,
including quoted values and trailing delimiter whitespace. Header folding,
non-binary transfer encoding, duplicate framing headers, incorrect Content-Length,
HTTP trailers, compressed batch envelopes and non-HTTP/1.1 parts are rejected.

One bounded blocking parser job runs at a time. Validated request bodies retain
shared slices of the bounded envelope, rather than duplicate object bodies.
Each response has a 512 KiB local budget; retained response data share a
10,000,000-byte envelope budget with space reserved for remaining error parts.
An oversized response is replaced by a per-part 413 responseTooLarge error that
states that the operation may have completed. These response limits and exact
malformed-input statuses are local behavior, without real-service parity claims.

The broad feature BEHAVIOR-COMMON-008 remains partial until final combined SDK
verification and the remaining provider/profile edge contract are reconciled.
This HTTP envelope is separate from the Storage Batch Operations service.

## Sources

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

tests/http_batch.rs covers metadata PATCH and atomic stale conditions, mixed
200/204/404/412 parts, optional and matching Content-IDs, encoded object names,
strict outer credentials with denied/invalid inner overrides, inner query
overrides, rejected inherited/inner query credentials, one connection slot,
100/101 calls, the 10 MB boundary, exact 64 KiB bodies, MIME framing,
invalid targets/headers/lengths and full-envelope validation before deletion.

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
