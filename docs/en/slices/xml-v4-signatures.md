# XML V4 HMAC and Authorization signatures

<!-- markdownlint-configure-file {"MD013": false} -->

This source-checkout slice covers DESIGN-031 and DESIGN-039 and extends the existing RSA query adapter. DESIGN-035 retains its unresolved provider duplicate/ambiguous-query question under SPEC-SIGNING-EDGES. Operation support, infrastructure simulation and the parent authentication rows remain separately tracked by the central feature ledger.

## Pinned contract

Official sources were read on 2026-10-31. Google pages report last update 2026-10-30; these are complete fetched HTML SHA-256 digests, rather than claims about a versioned Google API release:

| Source | SHA-256 |
| --- | --- |
| [Google signatures](https://docs.cloud.google.com/storage/docs/authentication/signatures) | `ee16ec35fe9496c78b87da5fb54c2c9950759abdf88183304cfab981829225b6` |
| [Google canonical requests](https://docs.cloud.google.com/storage/docs/authentication/canonical-requests) | `d4b7e058fc9b55258a8a007ab4b9056d7a422418938a2c250a25edaf5700869e` |
| [Google HMAC keys](https://docs.cloud.google.com/storage/docs/authentication/hmackeys) | `13b9bc24e1b504e6b0b70b5c601f144abf8c145abe0615ae80f5fae2b465f396` |
| [Google XML headers](https://docs.cloud.google.com/storage/docs/xml-api/reference-headers) | `3c564c341dfb599332bd8f35a1acc10d8ad557caf1af3f53b95c672da2330e9c` |
| [Google XML errors](https://docs.cloud.google.com/storage/docs/xml-api/reference-status) | `d30c6c5a06ae9064bc5a34ae2fc486c4b98f408494dac4fbda20650f85dc9053` |
| [AWS published S3 header vector](https://docs.aws.amazon.com/AmazonS3/latest/developerguide/sig-v4-header-based-auth.html) | `55aa3397d0c7aaffdcca49959ab851dbb9181fb7144dcea995522950b958d20c` |

The AWS example is an independent interoperability vector; Google defines the emulator contract. fake-gcs-server v1.56.1, revision `896ece51e49eed0946089f3089f6343855c0d106`, is only a comparison implementation. No real-cloud request is required or performed.

The official Rust stable manifest still identifies 1.99.1, dated 2026-10-15, with SHA-256 `fb152be80975c194aa5fc684b2aec3a6e14e2b855efe2faa4e5f8ae6e590f463`. Existing ring 0.17.14 supplies HMAC-SHA256 and constant-time verification. The [official sparse index](https://index.crates.io/ri/ng/ring) confirms the current non-yanked stable release, Rust 1.66 minimum and crate SHA-256 `a4689e6c2294d81e88dc6261c768b63bc4fcdb852be6d1352498b114f61383b7`. The JSON registry endpoint initially returned HTTP 403; the index and [published crate documentation](https://docs.rs/ring/latest/ring/) provide the successful verification. No dependency, schema or lockfile change is introduced by this slice.

## Request and identity boundaries

Google RSA and HMAC use `GOOG4-RSA-SHA256` and `GOOG4-HMAC-SHA256`, Google parameter/date/hash prefixes and `<date>/<location>/storage/goog4_request`. AWS-compatible HMAC uses `AWS4-HMAC-SHA256`, consistently `X-Amz-` parameters and `x-amz-date`/`x-amz-content-sha256`, with `<date>/<location>/s3/aws4_request`. These XML credentials never authenticate JSON, OAuth or bearer requests.

Query credentials require exactly one algorithm, credential, date, expiry, signed-header list and signature. The expiry is 1–604800 seconds, usable from 900 seconds early until strictly before expiry. Header signatures require unique Credential, SignedHeaders and Signature fields, one matching date header, matching scope day and an explicit content-sha256 header. Header time bounds include both endpoints of the documented 15-minute interval. Missing/unknown fields, mixed versions/prefixes and simultaneous query/header credentials fail closed in strict mode.

An ACTIVE key lookup uses the existing schema's unique access ID and static cached SQL. The returned service-account email must identify a registered strict principal, including HMAC-only registrations with a required but empty `tokens` array. Key ownership and key-admin permissions grant no object access. The resolved principal uses the shared IAM/ACL/UBLA/PAP checks. INACTIVE, DELETED, unknown and unregistered keys cannot authenticate; lookup failures retain their typed backend cause and return 503. Secrets are never cached or included in Debug/error output. Each newly authenticated request reads fresh state; an already admitted request is not cancelled by a later key revocation.

The 40-character creation secret is used literally as UTF-8 after the GOOG4/AWS4 prefix. Four HMAC-SHA256 steps consume the scope's date, location, service and terminator, then ring verifies the final signature in constant time. The base64-looking secret is never decoded.

## Canonicalization, bodies and resources

Verification uses the actual final Host, raw encoded path and full query before protocol decoding. Host is single and nonempty. Signed headers are lowercase, ordered and unique; repeated values retain wire order and normalize whitespace. All x-goog/x-amz headers except content-sha256 must be signed. The existing decoded-name query ordering and duplicate rejection remain visible local choices under SPEC-SIGNING-EDGES.

Query signatures use UNSIGNED-PAYLOAD. Header signatures use their explicit UNSIGNED-PAYLOAD or lowercase SHA-256 declaration. PUT, bucket creation and multipart completion compare the actual body before publication using existing bounded upload/metadata paths. Empty-body operations validate the empty digest and read actual frames through EOF; empty chunked framing is valid, nonempty frames and trailers are rejected. A checksum failure does not replace old bytes, metadata or generations.

Canonical input is bounded to 16 KiB of path/query, 64 headers and 64 KiB of header names/values. Crypto work uses one semaphore-admitted job on the existing bounded runtime blocking pool through task supervision. Connection admission bounds waiting network requests; the permit remains owned by the job even if its awaiting request is cancelled. No RSA work runs inside the catalog transaction and no extra pool, thread or durable publication step is introduced.

At this slice's original checkpoint, streaming/chunked and trailer signing were explicitly rejected. The subsequent [signed chunk upload slice](xml-signed-chunked-upload.md) implements the specific AWS4 HMAC body format directly linked by Google's [Content-Encoding and decoded-length contract](https://docs.cloud.google.com/storage/docs/xml-api/reference-headers#contentencoding), through ordinary XML object PUT. DESIGN-039 itself retains broader pending streaming operations and provider questions under SPEC-V4-HEADER-CHUNKED; ordinary header transfers alone never completed it. Signed-URL POST is documented only for resumable initiation; that XML operation remains unsupported and is rejected rather than reported successful. Authorization signatures support existing XML multipart POST operations. Form-policy extensions are tracked separately, and XML resumable initiation remains unimplemented. The explicitly permissive profile retains its documented absence of cryptographic guarantees.

## Acceptance and verification

The dedicated `xml_v4_hmac_http` target decomposes transfers, key state/restart, principal/project isolation, payload integrity, bucket/multipart operations, independent curl signing, RSA headers, malformed/mixed/JSON credentials, empty bodies/framing, and ACL/UBLA/PAP. Signature unit cases pin the published AWS vector, clock boundaries, credential shape, required headers, unsupported streaming fields, canonical ordering and resource caps. Existing RSA query, V2, signed form policy, JWT and XML regression targets remain required.

The scoped commands below passed on 2026-10-31 with Rust 1.99.1 and `CARGO_INCREMENTAL=0`. Every executed target reported zero ignored tests. Full SDK/container/integration gates remain reserved for final-source integration.

| Command or target | Result |
| --- | --- |
| `cargo check --locked --lib --test xml_v4_hmac_http` | PASS |
| `cargo test --locked --lib -- auth::signature xml:: storage::catalog::hmac_keys upload_reports_a_failed_final_write single_request_upload_failures_never_publish --test-threads=1` | PASS: 45 tests; signature 19, XML 20, HMAC catalog 4 and shared failed-write/fault-matrix 2 |
| `cargo test --locked --test auth_http signing:: -- --test-threads=1` | PASS: 33 tests, including JWT, RSA query, form policy, PAP and signed PUT |
| `cargo test --locked --test auth_http -- xml:: xml_put:: --test-threads=1` | PASS: 35 tests, including existing empty-body and hold denials |
| `cargo test --locked --test xml_v4_hmac_http --test xml_bucket_http --test xml_multipart_http --test xml_storage_http --test sign_url_cli --test project_resources_http -- --test-threads=1` | PASS: 11 + 13 + 8 + 14 + 7 + 5 = 58 tests |
| Dedicated `xml_v4_hmac_http` target after the early-body-rejection repair | PASS: 11 tests |
| `cargo clippy --locked --lib --bin gcs-mimic --test xml_v4_hmac_http -- -D warnings` | PASS |
| `cargo xtask lint` | PASS: zero production violations |
| Markdown lint 0.24.0 / markdownlint 0.42.0 on this slice and signed-download.md | PASS: two files, zero issues |

The first unit build failed before execution with `rustc-LLVM ERROR: IO failure on output stream: No space left on device`; central cleanup of reproducible build cache restored the test window. Initial wire failures exposed an invalid fixture-supplied Host and the old framing-only body guard. Moving that guard to actual frames then exposed a withheld-body DELETE timeout: `503 ServiceUnavailable` instead of `400 UnexpectedContent`. The final helper rejects a nonzero body size lower bound immediately and otherwise validates actual frames/EOF, preserving the original denial while accepting valid empty chunked/zero framing. The old RSA required-field unit also exposed an accidental expansion of its six-field constant; Google and AWS field sets are now distinct, with both used for credential detection. All affected checks were rerun successfully; no negative test, timeout, checksum or stack limit was weakened. npm registry lookup was unavailable, so Markdown lint used the already installed CLI directly.

## Acceptance index for the central ledger

All results below are scoped PASS results. They do not complete the pending operations or provider-exactness questions listed afterward.

| Feature / behavior | Exact test IDs |
| --- | --- |
| DESIGN-031: Google/AWS query GET/HEAD/PUT/DELETE on disk and RAM | `hmac_query_and_header_transfer_on_disk_and_memory` |
| DESIGN-031: ACTIVE state, restart, revocation, key-admin-only and cross-project/unregistered identity denial | `hmac_revocation_restart_and_identity_permissions_do_not_follow_key_project`; `storage::catalog::hmac_keys::tests::signing_lookup_never_caches_state_or_exposes_secrets_in_debug` |
| DESIGN-039: Google/AWS HMAC headers and RSA headers, required fields and payload declaration | `hmac_query_and_header_transfer_on_disk_and_memory`; `rsa_authorization_headers_and_existing_query_helpers_transfer_the_same_object`; `auth::signature::v4::tests::credential_prefix_scope_header_duplicates_and_streaming_fields_fail_closed` |
| DESIGN-031 / DESIGN-039: dates, exact expiry, mixed/duplicate credentials and JSON rejection | `auth::signature::v4::tests::header_fifteen_minute_and_query_early_expiry_boundaries_are_exact`; `malformed_mixed_tampered_and_expired_signatures_never_fall_back_or_reach_json` |
| DESIGN-031 / DESIGN-039: raw path/final Host, virtual host and repeated signed-header ordering | `hmac_virtual_hosts_raw_paths_and_repeated_signed_header_wire_order_are_bound`; `empty_chunked_frames_and_zero_lengths_match_payload_while_host_and_body_tampering_fail`; `auth::signature::v4::tests::canonicalization_bounds_resource_use_and_preserves_header_wire_order` |
| DESIGN-031 / DESIGN-039: body integrity and existing state preservation | `signed_payload_hash_mismatch_preserves_bytes_metadata_generation_and_count`; `signed_empty_body_operations_reject_nonempty_hashes_and_actual_payloads`; `xml::xml_delete_checks_permissions_conditions_and_empty_body` |
| DESIGN-031 / DESIGN-039: shared bucket/multipart authorization and IAM/ACL/UBLA/PAP | `hmac_bucket_and_multipart_operations_reuse_shared_catalog`; `hmac_acl_reader_ubla_pap_and_public_invalid_signature_preserve_authorization` |
| DESIGN-031 / DESIGN-039: independent interoperability and typed backend failure | `auth::signature::verify::tests::published_aws_s3_header_vector_verifies_without_a_local_signer`; `curl_aws_sigv4_independently_verifies_header_get_and_hashed_put`; `auth::signature::verify::tests::closed_catalog_retains_a_backend_cause_and_returns_xml_503` |

XML resumable initiation and its signed-URL POST exception, remaining streaming/trailer variants and provider-exact ambiguous/duplicate query semantics remain unimplemented or unresolved. HMAC form policy verification is covered by the subsequent [POST policy variant slice](xml-post-policy-variants.md), which retains the broader form-operation gaps. Recognized unsupported requests are rejected. SPEC-SIGNING-EDGES, SPEC-SIGNED-PAYLOAD and the authentication/operation parent rows retain those broader limits; no status is narrowed to hide them. Full SDK, container and real-provider differential checks were not run in this slice. No durable write/rename/fsync step changed: the existing write/flush/sync fault matrix was rerun, while crash and PowerLoss recovery matrices are left to final integration rather than claimed as newly executed evidence.

The independent local curl case requires `--aws-sigv4` capability and fails if absent. The observed tool is system curl/libcurl 8.7.1; this is a pinned observation, not a claim that it is the newest curl. Its [official option documentation](https://curl.se/docs/manpage.html#--aws-sigv4) and [8.7.1 signing source](https://github.com/curl/curl/blob/curl-8_7_1/lib/http_aws_sigv4.c) define the provider strings. Credentials pass through stdin configuration, with curlrc disabled, no proxy/redirect/retry and a 15-second timeout, only to the disposable loopback server.
