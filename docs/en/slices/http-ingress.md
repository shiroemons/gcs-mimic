# HTTP request compression and method override

The listener retries peer-specific accept failures immediately. Resource
exhaustion pauses acceptance for 10 ms, doubling to at most one second, while
shutdown and completed connections continue to be processed. A successful
accept resets the delay. Invalid/closed listener errors remain fatal and drain
the server. `accept_failed` logs the first resource failure and
`accept_recovered` logs recovery without repeated failure spam.

## Contract and scope

Gzip metadata and upload jobs share a per-server decoder pool sized to
`ceil(max_blocking_threads / 4)`, at least one. The default eight blocking
threads allow two decoder jobs while leaving capacity for file/catalog I/O.
Separate State instances do not share permits. No performance gain is claimed.

The JSON API accepts transport gzip on metadata requests under /storage/v1/.
It also decodes the JSON metadata of a resumable initiation, a POST to
/upload/storage/v1/b/{bucket}/o with uploadType=resumable that names no session
once its query keys are decoded (`upload%5Fid` names one, as on Cloud Storage).
Session chunk requests retain their existing Content-Encoding rejection and
invalid-session behavior; transport compression never sets stored contentEncoding.
Compressed input and expanded input each have the 16 MiB JSON limit, so a small
compressed body cannot grow past it. Concatenated gzip
members are decoded as one representation. Invalid headers or deflate data,
truncated members (including a missing 8-byte trailer), trailing garbage and
HTTP trailers fail before routing any metadata mutation. The trailer's CRC-32
and ISIZE are read but not compared with the decoded data, as on Cloud Storage.
Expansion runs as one bounded blocking job at a time; the existing connection
admission bounds waiting requests.

Cloud Storage's front end expands a compressed body before it routes or
authenticates the request (observed 2026-11-25, see
[transport gzip observations](../evidence/transport-gzip-provider-observations.md)),
and gcs-mimic does the same: a body that does not decode answers 400 with the
`text/plain; charset=utf-8` text `Could not decompress request body.`, whoever
sends it and whatever route it names, with the CORS headers of a CORS route. A
body that decodes then meets the same checks, in the same order, as the same
body sent plain. An empty body is not decoded and takes no decoder permit.
Every caller's compressed JSON body therefore waits for a decoder permit, and
the 16 MiB limits bound what any caller can make the server expand.

A media or multipart upload body streams to its handler instead of being
expanded first. Its first bytes are checked before dispatch, so a body that does
not start a gzip member answers 400 before the caller is checked. A failure
later in the body replaces the handler's answer with the same 400 once the
handler reads it. A handler that refuses the caller before it reads the body
keeps its refusal, where Cloud Storage reads the body to its end and answers 400
for a member cut short; reading an upload of up to max_object_bytes from a
refused caller only to report its encoding is the difference gcs-mimic does not
reproduce.

This is request transport decoding. Object contentEncoding, download
transcoding and XML signed-request canonicalization retain their existing
meanings. POST uploads with uploadType=multipart also accept transport gzip:
the entire multipart/related envelope is decoded before the existing streaming
metadata/media parser. POST uploads with uploadType=media stream their body
through the same decoder and store the decoded bytes, matching Cloud Storage
(observed 2026-10-31, see
[media read observations](../evidence/media-read-provider-observations.md)); a
contentEncoding query parameter still sets the stored metadata independently.

Multipart decoding consumes and produces at most 64 KiB per blocking step,
with one output channel slot and the same single decoder permit as metadata.
Expanded input is bounded by max_object_bytes plus two 16 MiB JSON parts and
256 KiB of MIME overhead; compressed input allows that budget plus one eighth and 64 KiB for deflate
overhead. These are local resource limits, not Google API limits. With the default
max_object_bytes of 5 TiB a small gzip body can expand that far, so lower the setting on a
shared server or one listening on `0.0.0.0`. Media itself
retains the configured max_object_bytes cap and incremental checksum checks.
The complete media or envelope is never buffered in memory.

The request body reports clean EOF only after the gzip decoder has read every
member's complete trailer and transport EOF. A dropped or panicking producer reports an
error even if it already sent the closing MIME boundary. The existing multipart
finish/EndWithHashes protocol withholds publication until that validated EOF.
Truncated members, trailing garbage, HTTP trailers, disconnect and deadlines
therefore preserve an existing object and reclaim staging. Receiver cancellation
stops the producer; a blocking step can finish independently but has bounded work.
Stored contentEncoding=gzip is metadata for the media part and remains distinct
from the outer transport header, including doubly compressed requests.

One X-HTTP-Method-Override: PATCH header on a POST selects the existing JSON
bucket, object or ACL PATCH handler. Routing, validation, strict authorization
and atomic preconditions see PATCH. Other methods, override values, duplicate
headers and XML/upload/internal paths reject the override.

Both compatibility profiles use the same limits and strict permissions.
The canonical feature ledger remains authoritative; this document does not
promote broader JSON operation rows to complete.

## Sources

Checked on 2026-10-30:

- [Cloud Storage JSON API](https://docs.cloud.google.com/storage/docs/json_api),
  section “Alternate notation when PATCH HTTP verb is not supported”; retrieved
  HTML SHA256 1127df36db7c3db98096685d4f2c71a3d0820ecce6aa9d2700da6040e728eae0.
- [Reference server source](https://github.com/fsouza/fake-gcs-server/blob/896ece51e49eed0946089f3089f6343855c0d106/fakestorage/server.go),
  revision 896ece51e49eed0946089f3089f6343855c0d106 (v1.56.1), retrieved
  SHA256 abe448cd575fbc5687a2527031c3b72fc1ff4b109b12333d1b1d031ab533c005.
  Its gzip middleware is comparison evidence; its error and resource behavior
  does not override the bounded local contract.
- [Java MediaHttpUploader](https://github.com/googleapis/google-api-java-client/blob/9e0a9f014ae0a0a6db5e94fbbff6d55eec958f82/google-api-client/src/main/java/com/google/api/client/googleapis/media/MediaHttpUploader.java),
  revision 9e0a9f014ae0a0a6db5e94fbbff6d55eec958f82, SHA256
  a493bd78196d0c465122ce4621bdd9b66030fc196d6874b2ea27d94f3e624e4b.
  Direct upload constructs a MultipartContent from metadata and media and then
  sets GZipEncoding on that HTTP request by default. This establishes envelope
  transport compression independently of the stored object encoding.
- [Google JSON resumable initiation](https://docs.cloud.google.com/storage/docs/performing-resumable-uploads),
  retrieved HTML SHA256
  f7a01d2b7450b3129a158d0075a21ab33bded0179bf029eb92eaacebd9872a03.
  The initiation POST body is JSON metadata, separate from subsequent object bytes.
- [Official .NET resumable uploader](https://github.com/googleapis/google-api-dotnet-client/blob/dd7f8ffe5a9aa3b9b07174dc03faf0f3289e7e24/Src/Support/Google.Apis/Upload/ResumableUpload.cs),
  Google.Apis 1.76.0, revision dd7f8ffe5a9aa3b9b07174dc03faf0f3289e7e24,
  SHA256 9d0a79db44f2cbf794cf9a685d68ccf389ef5900c037091e2ac4cd711b3b7703.
  CreateInitializeRequest serializes initiation metadata through the service.
  [BaseClientService](https://github.com/googleapis/google-api-dotnet-client/blob/dd7f8ffe5a9aa3b9b07174dc03faf0f3289e7e24/Src/Support/Google.Apis/Services/BaseClientService.cs)
  defaults GZipEnabled to true (SHA256
  0c1050d8ebce316767fc0d1e1b7666b6d2ad1c75e06f316f24af9f1e502be484).
  Its [serializer extension](https://github.com/googleapis/google-api-dotnet-client/blob/dd7f8ffe5a9aa3b9b07174dc03faf0f3289e7e24/Src/Support/Google.Apis/Requests/HttpRequestMessageExtenstions.cs)
  compresses JSON and sets Content-Encoding: gzip (SHA256
  d6d90dd9cbeb3f25cb9abaed0c855470669332f7bebcc988c4410bde0229101d).
  Actual Google.Cloud.Storage.V1 5.0.0 with Gax.Rest 4.13.1 sent this header
  and a 146-byte gzip initiation body without disabling SDK compression.

## Acceptance

The tests in tests/it_protocol/http_ingress.rs cover compressed bucket creation, compressed
object PATCH, POST override, preserved object bytes, stale atomic conditions,
strict denial, malformed/truncated/trailing input, accepted CRC/ISIZE mismatches,
concatenated members, HTTP trailers and compressed/expanded boundaries. Uncompressed media and
metadata are exercised in the same workflow. `gzip_and_plain_bodies_meet_the_same_refusal`
sends a CORS bucket PATCH, a rewrite and an object PATCH by callers without permission, a
media upload with an unparsable condition, an initiation whose query spells `upload_id` with an
escape and an unknown route, plain and compressed, and requires the same status, body and CORS
headers; an anonymous body that does not decode answers 400 with CORS headers.
`gzip_expands_before_authentication_and_override_uses_patch_permissions` requires 413 for a
bomb and 400 for a body that does not decode from callers who would be refused.
`server::ingress::tests::a_gzip_body_is_expanded_before_the_caller_is_checked` holds every
decoder permit: requests without a body and an upload whose first bytes are not gzip answer at
once, while refused, unknown-route and initiation requests with a compressed body wait.
`server::ingress::members::tests` cover optional header fields, concatenated members, ignored
trailer fields, each refusal and a read resumed after its input runs dry.

tests/it_protocol/http_multipart_gzip.rs additionally covers a media stream over 2 MiB, one-byte
HTTP chunks and concatenated gzip members, stored gzip within transport gzip,
truncation and garbage after the complete MIME boundary, accepted CRC/ISIZE mismatches, HTTP
trailers, object limits, checksum mismatch, strict denials, disconnect,
absolute 408 deadlines and staging cleanup. A media or multipart body that is not gzip
answers 400 before the caller is checked, and a refused caller whose body is cut short later
keeps its refusal.

tests/it_protocol/http_resumable_gzip.rs covers compressed and concatenated initiation
metadata, ordinary chunk bytes, separate stored encoding, existing chunk-error
invalidation, truncation/garbage, accepted CRC/ISIZE mismatches, duplicate and non-gzip codings,
HTTP trailers, compressed/expanded 64 KiB boundaries and strict permissions.
Rejected initiations leave the session catalog and staging unchanged. The four
tests pass, as do the four existing ingress and six multipart-gzip wire tests.
Unfiltered cargo check, targeted Clippy and the production lint gate pass.
The actual .NET SDK passes gzip initiation, three chunks and exact read after
the fix; this does not establish its later metadata-update or complete SDK gate.

On 2026-10-30, cargo check and cargo check --tests passed, and
cargo test --test http_ingress passed all four wire tests. The existing
cargo test --lib server::upload_frame_tests passed both frame-bound tests.
The multipart transport extension passed all six dedicated wire tests.
The two server::ingress::upload_gzip::tests cases also passed, covering explicit
EOF after an injected producer panic and compressed/expanded stream budgets.
The final production lint gate reported zero violations. The release-input
dependency check verified the pinned Rust 1.99.1 toolchain and all release inputs
as current; this check intentionally excludes development-only SDK inputs.
The final combined HTTP command
cargo test --test http_batch --test http_ingress --test http_multipart_gzip
--test object_multipart_http passed 36 tests, including 22 existing multipart
regressions. The two existing server::upload_frame_tests cases passed as well.
Owned Rust formatting, Markdown lint for both HTTP slice documents and
git diff --check passed.
SDK, container and full integration checks belong to the final combined source
verification. The fake-gcs-server profile and ACL-specific override wire cases remain
unverified by this slice.
