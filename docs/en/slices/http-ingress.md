# HTTP request compression and method override

## Contract and scope

The JSON API accepts transport gzip on metadata requests under /storage/v1/.
It also decodes JSON metadata for exact POST requests to
/upload/storage/v1/b/{bucket}/o with uploadType=resumable and no upload_id.
Session chunk requests retain their existing Content-Encoding rejection and
invalid-session behavior; transport compression never sets stored contentEncoding.
Compressed input and expanded input each have a 64 KiB limit. Concatenated gzip
members are decoded as one representation. Invalid headers, deflate data, CRC32,
ISIZE, truncated members, trailing garbage and HTTP trailers fail before routing
any metadata mutation. Expansion runs as one bounded blocking job at a time;
the existing connection admission bounds waiting requests.

This is request transport decoding. Object contentEncoding, download
transcoding and XML signed-request canonicalization retain their existing
meanings. POST uploads with uploadType=multipart also accept transport gzip:
the entire multipart/related envelope is decoded before the existing streaming
metadata/media parser. Raw-media transport gzip is outside this scope and keeps
its existing unsupported Content-Encoding error.

Multipart decoding consumes and produces at most 64 KiB per blocking step,
with one output channel slot and the same single decoder permit as metadata.
Expanded input is bounded by max_object_bytes plus 256 KiB of MIME overhead;
compressed input allows that budget plus one eighth and 64 KiB for deflate
overhead. These are local resource limits, not Google API limits. Media itself
retains the configured max_object_bytes cap and incremental checksum checks.
The complete media or envelope is never buffered in memory.

The request body reports clean EOF only after the gzip decoder validates all
members, trailers and transport EOF. A dropped or panicking producer reports an
error even if it already sent the closing MIME boundary. The existing multipart
finish/EndWithHashes protocol withholds publication until that validated EOF.
CRC/ISIZE failures, trailing garbage, HTTP trailers, disconnect and deadlines
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

The tests in tests/http_ingress.rs cover compressed bucket creation, compressed
object PATCH, POST override, preserved object bytes, stale atomic conditions,
strict denial, malformed/truncated/CRC/ISIZE/trailing input, concatenated members,
HTTP trailers and compressed/expanded boundaries. Uncompressed media and
metadata are exercised in the same workflow.

tests/http_multipart_gzip.rs additionally covers a media stream over 2 MiB, one-byte
HTTP chunks and concatenated gzip members, stored gzip within transport gzip,
invalid CRC/ISIZE/truncation/garbage after the complete MIME boundary, HTTP
trailers, object limits, checksum mismatch, strict denials, disconnect,
absolute 408 deadlines and staging cleanup. Raw media with Content-Encoding
retains its explicit unsupported error instead of being silently decoded.

tests/http_resumable_gzip.rs covers compressed and concatenated initiation
metadata, ordinary chunk bytes, separate stored encoding, existing chunk-error
invalidation, CRC/ISIZE/truncation/garbage, duplicate and non-gzip codings,
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
verification. Legacy-profile and ACL-specific override wire cases remain
unverified by this slice.
