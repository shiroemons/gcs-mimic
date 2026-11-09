# Client compatibility checks for 0.16.0

Recorded on 2026-11-09 with Rust 1.99.1.

## Cloud Storage observations

Differential cases came from disposable private buckets through `cargo xtask differential run --record` and are stored in `compatibility/differential/reference/gcs.json`. The harness deleted every bucket it created.

| Case | Observation |
| --- | --- |
| `batch-part-framing` | CRLF with a blank line, CRLF without one, LF-only lines and a request line directly before the delimiter all answered an outer 200 and an inner 204, and the object was deleted |
| `xml-predefined-acl` | `private`, `project-private`, `bucket-owner-read` and `bucket-owner-full-control` answered 200 with the expected entries, the object owner first. A JSON spelling and an unknown value answered 400 `InvalidArgument` with `Invalid canned acl: VALUE`. Uniform bucket-level access answered 400 with the legacy ACL message |
| `obj-predefined-acl-order` | A JSON upload with a predefined ACL also lists the object owner first. An object that inherits the default ACL lists the owner last (`bkt-acl-crud`) |
| `xml-amz-headers` | With an OAuth token, `x-amz-meta-*` answered 200 and stored nothing, `x-amz-acl` and `x-amz-storage-class` answered 200, and `x-amz-copy-source` answered 400 `Missing copy source` |
| `json-key-parameter` | A made-up `key` with an OAuth token answered 200 on object get, list, upload and bucket get |
| `obj-metadata-scalars`, `obj-metadata-nonscalar`, `obj-metadata-empty-map` | Numbers and booleans are stored as text. Object and array values are dropped while other keys apply. A PATCH that leaves no entries, including an empty map, clears the custom metadata |
| `obj-metadata-number-forms`, `obj-metadata-number-bounds` | Integers within 64 bits keep their digits. Other numbers are stored in text like `1.0E21`, `9.9E-4` and `1000000.0`, with `0.0E` or `-0.0E` for a float zero and 16 significant digits from 2^64 upward, so integers beyond 64 bits lose digits on the service too |
| `obj-json-response-overrides` | `response-content-disposition` and `response-content-type` answered 200 on object GET and media download, without being applied |
| `obj-unencoded-name` | An unencoded `=` answered 200 and an unencoded `/` answered 404 |

One further run used a temporary service account whose only role was on a disposable bucket, with a temporary HMAC key, `curl --aws-sigv4` and locally computed V4 signatures. The account, key and bucket were deleted afterward.

| Request | Cloud Storage |
| --- | --- |
| AWS4 PUT with `x-amz-meta-*`, then GET or HEAD | 200, and the read answers `x-amz-meta-origin` |
| AWS4 PUT with `x-amz-storage-class: NEARLINE` | 200, stored as NEARLINE |
| AWS4 copy with `x-amz-copy-source`, with and without `x-amz-metadata-directive: REPLACE` | 200 and the expected metadata and content type |
| AWS4 PUT with an `x-amz-*` and `x-goog-*` spelling of one header | 400 `ExcessHeaderValues`, `Requests cannot specify both x-amz and x-goog headers` |
| AWS4 PUT with `x-amz-server-side-encryption: AES256`, an unknown `x-amz-*` header or a bare `x-amz-metadata-directive` | 200 without effect |
| AWS4 PUT with `x-amz-acl: aws-exec-read` | 400 `InvalidArgument`, `Invalid canned acl: aws-exec-read` |
| V4 query signature with lowercase or mixed-case names | GET 200, PUT 200, resumable POST 201 |
| The same URL with names signed in another case | 403 `SignatureDoesNotMatch` |
| `x-goog-date` repeated as `X-Goog-Date` | 400 `ExcessHeaderValues`, Details `x-goog-date` |

The following were not exercised against Cloud Storage: `gsutil signurl` and `gcloud storage sign-url` output, V2 `AWS` or AWS4 query signatures with `x-amz-*` headers, customer-key encryption and grant headers, and an anonymous request with a `key`.

## Local checks

| Command | Result |
| --- | --- |
| `cargo xtask differential run --reference compatibility/differential/reference/gcs.json --fail-on-diff --fail-on-note` | 73 cases match, no notes |
| `cargo test --locked --offline --lib -- auth::signature xml::error_tests` | passed |
| `cargo test --locked --offline --test it_xml_auth cli_query_names` | passed |
| `cargo test --locked --offline --test it_xml_auth xml_amz_headers` | passed |
| `cargo test --locked --offline --test it_xml_auth xml_canned` | passed |
| `cargo test --locked --offline --test it_protocol http_batch` | passed |
| `cargo test --locked --offline --lib batch` | passed |
| `cargo test --locked --offline --test it_objects api_key` | passed |
| `cargo test --locked --offline --test it_objects custom_metadata_values` | passed |

## gsutil

gsutil 5.38 ran without credentials against gcs-mimic over TLS, in the native and `legacy-emulator` profiles. `mb`, `cp` up (small and 9 MiB), `cp` down (`cmp` equal), `ls` and `stat` all exited 0. Debug output showed `key=` on the requests and no `Authorization` header. `rm`, `rb`, bucket-to-bucket `cp` and `rsync` were not run.
