# XML V4 Authorization signed chunk uploads

<!-- Exact vectors and reproducible identifiers stay on table rows. -->
<!-- markdownlint-configure-file {"MD013": {"tables": false}} -->

This source-checkout slice implements the AWS4 HMAC chunk chain explicitly
linked by Google's XML Content-Encoding contract. It extends DESIGN-039 and
the upload/payload behavior in DESIGN-037 and EXT-XML-022. Ordinary XML object
PUT is the implemented operation; other upload operations and provider questions
remain part of the original parent scope, not excluded S3-only requirements.

## Primary contract and reconciliation

Checked on 2026-10-31; digests identify the complete retrieved HTML documents.

| Source | SHA-256 |
| --- | --- |
| [Google XML headers](https://docs.cloud.google.com/storage/docs/xml-api/reference-headers#contentencoding) | `4ba58910ca14c3c6fabd321d381e66e41f74bd0c0c57d3ea908523faeeb0ba85` |
| [AWS streaming body specification directly linked by Google](https://docs.aws.amazon.com/AmazonS3/latest/developerguide/sigv4-streaming.html) | `27ae348861af1378cbacf7a87cc2ae4f174e5f9102c8fcfc7a5d6d20fc233c4f` |

Google's [Transfer-Encoding](https://docs.cloud.google.com/storage/docs/xml-api/reference-headers#chunked)
and [decoded-length](https://docs.cloud.google.com/storage/docs/xml-api/reference-headers#xamzdecodedcontentlength)
sections require `x-amz-decoded-content-length` for V4 Authorization uploads
using HTTP chunked transfer. It must equal the decoded object size, and x-goog
extension headers cannot be used. Content-Encoding `aws-chunked` is recommended.
When that encoding is used, Google directly requires the linked AWS body format
and excludes the aws-chunked token from stored metadata. The old AWS API URL
redirects; the developer-guide URL above is the pinned contract's actual page.

The generic full SHA256/UNSIGNED-PAYLOAD header description does not itself
define per-chunk signatures. The linked format instead defines the exact
`STREAMING-AWS4-HMAC-SHA256-PAYLOAD` canonical payload marker, an AWS4 seed
signature, chained HMAC signatures and a signed zero-length terminal chunk.
This specific linked format supplies the implementation basis; it does not
establish every AWS streaming, trailer or SigV4a variant as Google behavior.

| Request form | Implemented handling |
| --- | --- |
| AWS4 Authorization, streaming marker, decoded length, Content-Encoding aws-chunked, HTTP Content-Length | Decode the signed content chunks; Content-Length measures encoded HTTP body bytes |
| Same signed content with HTTP Transfer-Encoding chunked | The transport first removes HTTP framing; the adapter verifies the signed content framing |
| Streaming marker and decoded length, HTTP Transfer-Encoding chunked, no aws-chunked encoding | Decode the explicit signed content framing; omitting the recommended encoding does not discard the marker |
| AWS4 Authorization, full SHA256 or UNSIGNED-PAYLOAD, decoded length, HTTP Transfer-Encoding chunked, no aws-chunked encoding | Ordinary transport-decoded object bytes; exact decoded length and supplied full hash are checked |
| Marker with query credentials, Google RSA/HMAC prefix, non-PUT or multipart route | Explicit rejection; no verification context is discarded and no successful raw encoded upload is returned |
| Trailer markers, announced trailers or actual trailer frames | Explicit rejection; no publication |

The no-encoding interpretation follows the combination of Google's recommended
encoding wording and the explicit canonical payload declaration. Its exact
provider behavior remains a contract question; the positive local cases do not
resolve that question by assertion. RSA/query/chunked multipart behavior also
remains within DESIGN-039's pending scope until primary evidence and tests
establish it. Google extension aliases are not invented for decoded length.

Rust's official stable manifest still resolves to 1.99.1, dated 2026-10-15,
SHA-256 `fb152be80975c194aa5fc684b2aec3a6e14e2b855efe2faa4e5f8ae6e590f463`.
The official registry's current non-yanked stable ring release is 0.17.14,
checksum `a4689e6c2294d81e88dc6261c768b63bc4fcdb852be6d1352498b114f61383b7`.
No dependency, schema or durable publication step was added. fake-gcs-server
v1.56.1, revision `896ece51e49eed0946089f3089f6343855c0d106`, remains a
comparison implementation rather than this feature's contract.

## Authentication, streaming and publication

The raw final Host/path/query and original headers reach V4 seed verification
before adapter decoding or metadata normalization. Mixed credentials, duplicate
or missing signing/decoded-length fields, x-goog extension mixing and unknown
algorithms fail closed. The current ACTIVE key lookup occurs once per newly
authenticated request and resolves a registered principal. The secret is used
literally as UTF-8 with the AWS4 prefix and four scope derivation rounds. Key
ownership/project and key-administrator authority grant no object permission.
There is no anonymous fallback, including under public grants.

The request-local chunk verifier holds the derived key, date/scope and previous
signature privately, without Debug, logs, argv or persistent/global key cache.
It uses ring's constant-time HMAC verification and the same semaphore-admitted,
supervised blocking jobs as ordinary RSA/HMAC verification. No new pool, thread
or unbounded queue is created. An admitted request is not claimed to be cancelled
by later key revocation. Shared IAM/ACL/UBLA/PAP authorization and atomic
create/overwrite publication still apply to the authenticated principal.

The parser retains one bounded transport frame, a maximum 97-byte chunk header,
one incremental SHA256 context and fixed-size signature state. Data is sent in
at most 64 KiB parts through the existing two-slot upload bridge. It never
collects a whole signed chunk or object. The linked format's non-final data
chunks must be at least 8 KiB; the final data chunk may be smaller. This bounds
the number of headers and crypto jobs. Checked encoded input is limited to
`decoded + (decoded / 8192 + 2) * 102` bytes with checked arithmetic. Chunk sizes
are bounded hexadecimal u64 values and cannot exceed remaining decoded bytes.

Unverified object data can enter the existing staging upload, but the adapter
sends UploadChunk::End only after every data signature, signed zero chunk,
exact decoded size and actual transport EOF pass. Extra bytes, trailer frames,
truncation or read failures abort the upload. Cleanup is awaited and catalog
failures keep precedence over an adapter cancellation. The configured maximum
object size counts decoded bytes; Content-MD5 uses those same stored bytes.
Only the aws-chunked encoding token is removed. Other encoding tokens remain
metadata and their bytes are not decompressed by the upload adapter.

Existing storage commits, fsyncs and recovery are reused. No new rename/fsync
or crash boundary was introduced. A committed old resource is not changed by
failed authentication, chunk/hash/framing/length validation or failed writes.
The explicitly permissive profile's ordinary authentication behavior is
unchanged; the new cryptographic streaming mode requires a registered key.

## Acceptance and evidence

The exact test identifiers below separate the new wire cases from the existing
ordinary-signature regression target that hosts them.

| Behavior | Exact test ID |
| --- | --- |
| Public independent 66560-byte AWS seed and all three chunk signatures | `auth::signature::streaming::tests::published_aws_streaming_seed_and_three_chunk_signatures_match_exactly` |
| Disk/RAM, encoded Content-Length, outer HTTP chunked, multiple content chunks, empty object, one-byte framing splits, multiple chunks per transport part, large chunk and raw encoded path | `xml_v4_streaming::signed_chunks_transfer_binary_and_empty_objects_over_content_length_and_transfer_encoding` |
| No aws-chunked encoding, full SHA256 or UNSIGNED-PAYLOAD transport body | `xml_v4_streaming::google_transfer_encoding_without_aws_content_encoding_uses_full_hash_or_unsigned_decoded_body` |
| Data/signature/terminal tampering, truncation, extra bytes, actual trailers, decoded under/over length, complete old resource and restart/staging | `xml_v4_streaming::signed_chunk_tampering_lengths_terminal_eof_and_trailers_preserve_the_complete_old_resource` |
| Content-MD5 over decoded gzip bytes and removal of only the aws-chunked encoding token | `xml_v4_streaming::signed_chunks_validate_decoded_md5_and_remove_only_the_aws_encoding_token` |
| Exact decoded max and max+1, missing/duplicate/overflow/mixed headers, chunk header cap and tiny non-final chunks | `xml_v4_streaming::signed_chunks_enforce_decoded_object_limit_and_reject_malformed_header_and_chunk_resources` |
| Creator-only create/overwrite, reader/key-admin/unknown/cross-project, PAP/UBLA scoped IAM/revocation and key state/restart | `xml_v4_streaming::signed_chunks_use_current_keys_and_principal_permissions_with_creator_overwrite_and_pap_ubla` |
| Invalid seed rejects before 100 Continue under public grants; non-target multipart context is never discarded | `xml_v4_streaming::invalid_streaming_seed_and_non_object_routes_fail_before_continue_without_public_fallback` |
| Initial/tail write, flush/sync faults, complete old resource/count/bytes/staging and restart | `xml::put::streaming::tests::signed_chunk_upload_write_tail_flush_and_sync_failures_preserve_resource_and_restart` |
| Closed-catalog XML 503 with a retained typed verifier cause | `xml::put::streaming::tests::streaming_key_lookup_failure_keeps_its_backend_cause` |

The published AWS example fixes the seed and chunk signatures below. The unit
verifies the seed through production canonicalization and key derivation, then
compares each published chunk signature without a local signer generating the
expected values. Wire fixtures sign independently of the production decoder.

| Input | Published signature |
| --- | --- |
| Seed | `4f232c4386841ef735655705268965c44a0e4690baa4adea153f7db9fa80a0a9` |
| 65536 bytes of a | `ad80c730a21e5b8d04586a2213dd63b9a0e99e0e2307b0ade35a65485a288648` |
| 1024 bytes of a | `0055627c9e194cb4542bae2aa5492e3c1575bbb81b612b7d234b86a503ef5497` |
| Zero terminal | `b6c6ea8a5354eaf15b3cb7646744f4275b71ea724fed81ceb9323e279d449df9` |

## Verification and remaining scope

All Cargo invocations use `CARGO_INCREMENTAL=0`. Unfiltered test-configuration
checks passed before the final targeted tests. No test was ignored. The
following results were recorded on 2026-10-31.

| Command | Result |
| --- | --- |
| `cargo check --locked --lib --tests` | PASS |
| `cargo test --locked --lib -- auth::signature xml:: storage::catalog::hmac_keys upload_reports_a_failed_final_write single_request_upload_failures_never_publish --test-threads=1` | PASS: 51 tests, including fixed vector and new streaming fault/cause cases |
| `cargo test --locked --test xml_v4_hmac_http --test xml_post_policy_http --test xml_bucket_http --test xml_multipart_http --test xml_storage_http --test sign_url_cli -- --test-threads=1` | PASS: 18 + 9 + 13 + 8 + 14 + 7 = 69 tests |
| `cargo test --locked --test auth_http -- signing:: xml:: xml_put:: --test-threads=1` | PASS: 68 tests |
| Independent `cargo test --locked --offline --test xml_v4_hmac_http xml_v4_streaming:: -- --test-threads=1` | PASS: seven new wire tests, zero ignored |
| `cargo clippy --locked --lib --bin gcs-mimic --test xml_v4_hmac_http --test xml_post_policy_http --test auth_http -- -D warnings` | PASS |
| `cargo test --locked --lib -- auth::signature::streaming:: xml::put::streaming:: --test-threads=1` after shared helper extraction | PASS: three tests |
| `cargo test --locked --test xml_v4_hmac_http xml_v4_streaming:: -- --test-threads=1` after shared helper extraction | PASS: seven tests |
| `cargo xtask lint` after shared helper extraction | PASS: zero production violations, one existing reasoned suppression |
| Targeted `rustfmt --edition 2024 --check` | PASS |
| Markdown lint 0.24.0 / markdownlint 0.42.0 on this slice, xml-v4-signatures.md and signed-urls.md | PASS: three files, zero issues |

Independent source/caller/decoder review found no blocking or major issue. The
ordinary object path
named new-bucket in a negative fixture is not evidence of a dedicated bucket
creation request: only the multipart-specific context rejection is claimed by
that wire test. Other generic handlers reject the marker in source and remain
pending as supported streaming operations.

Initial test-only module
placement triggered Cargo integration-target autodiscovery errors, corrected by
placing the child under its matching module directory. A fixture attempted to
call a private base64 helper; it now encodes its test digest locally without a
production visibility change. A shared catalog edit temporarily stopped builds
with an unresolved principals binding; its owner corrected it. A new backend
fixture incorrectly demanded a nested source from the root CatalogError; it was
corrected to assert VerificationError::Catalog and its retained source instead.
None of those compile/test failures was counted as passing evidence.
An intermediate production lint run found four function-length violations in
shared catalog code. Its owner extracted the affected helpers separately and
reported the successful final shared check and production lint above. That gate
was then independently reconfirmed for this slice after another successful
test-configuration check, together with the three streaming unit/fault tests and
seven new wire tests. The wider 188 scoped tests preceded those final shared
catalog helper extractions; full final-source integration remains a separate
coordinated gate.

Google provider-exact marker/no-encoding behavior, RSA/query streaming formats,
multipart/bucket/resumable streaming operations, trailer variants, optional
payload semantics, exact provider errors and strict SDK interoperability remain
pending. The AWS example's storage-class header is part of the crypto vector,
not a claim that the object adapter implements that unrelated storage class.
Full SDK, container, full Rust and crash/PowerLoss suites remain reserved for
final-source integration. No test skip, larger stack or weakened authorization,
checksum or durability rule is used. Central feature/gap/matrix updates belong
to their maintainer; broad parent features must not be marked complete from this
ordinary-PUT slice alone.
