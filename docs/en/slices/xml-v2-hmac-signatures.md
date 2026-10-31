# Legacy V2 HMAC queries and signed headers

<!-- Exact source pins, vectors and commands remain on single table rows. -->
<!-- markdownlint-configure-file {"MD013": {"tables": false}} -->

This slice extends DESIGN-040 with GoogleAccessId/Expires/Signature HMAC query
authentication and GOOG1/AWS Authorization headers. XML object and the tested
bucket operations reuse registered principals, current key state and shared
authorization. Trusted virtual-host selection supplies the bucket for both
existing RSA and new HMAC V2 canonical resources. DESIGN-040 remains PARTIAL:
POST's broader forms, signed resumable operations and provider/SDK questions
remain in their original scope.

## Primary sources and algorithm reconciliation

Sources were checked on 2026-10-31. Documentation digests identify the complete
retrieved HTML, including presentation content.

| Source | Pin or SHA-256 |
| --- | --- |
| [Google V2 signing](https://docs.cloud.google.com/storage/docs/access-control/signed-urls-v2) | `ad0ee0ee8c37463f888d93c2293a8fe179e04680e6123c456d92b9eda017249c` |
| [Google XML Authorization](https://docs.cloud.google.com/storage/docs/xml-api/reference-headers#authorization) | `1cd632ad7c02efd2cbcb348d33bf92b29a1e4a3b1744b39bab494c905ca5749f` |
| [Google simple migration](https://docs.cloud.google.com/storage/docs/aws-simple-migration) | `afb960e1fc7b9a03f4ba9e04dcc4afb097d483b5a8b5c22d07e19846fe4bd1ad` |
| [Google gsutil revision](https://github.com/GoogleCloudPlatform/gsutil/tree/368796d77ac3c0996cf85ffc0285bf786e2a6fe3) | `368796d77ac3c0996cf85ffc0285bf786e2a6fe3` |
| [gsutil dependency pin](https://github.com/GoogleCloudPlatform/gsutil/blob/368796d77ac3c0996cf85ffc0285bf786e2a6fe3/.gitmodules) | Boto gitlink `51c16de7a27cb482e5478018d9c4b53577c1eba9`; .gitmodules digest `e88565ceb90e7eb0cbfeef39fba26d6375515a3c21a57c261fbfe22144080f36` |
| [Pinned Boto HmacAuthV1Handler](https://github.com/gsutil-mirrors/boto/blob/51c16de7a27cb482e5478018d9c4b53577c1eba9/boto/auth.py) | `a1d3f2f9ffc82371ed622a15bfd8209744272719aa522f0a83773d2f0af56ce8` |
| [Pinned Google connection](https://github.com/gsutil-mirrors/boto/blob/51c16de7a27cb482e5478018d9c4b53577c1eba9/boto/gs/connection.py) | `6f20b02d841ee1405c538722173b044472b7535cdbf29247ed7dcb42ba1b3fd6` |
| [Pinned query generation](https://github.com/gsutil-mirrors/boto/blob/51c16de7a27cb482e5478018d9c4b53577c1eba9/boto/s3/connection.py) | `8175d5c699b57c7b6365f6f7ad4566ee0963ce65ab42f168ed34f7c09ceeca9a` |
| [Pinned Google provider](https://github.com/gsutil-mirrors/boto/blob/51c16de7a27cb482e5478018d9c4b53577c1eba9/boto/provider.py) | `137a64bc5b0c924186d9865adfd5a98f9707db9df53ce524d7d072caabf68d71` |
| [Pinned canonical string](https://github.com/gsutil-mirrors/boto/blob/51c16de7a27cb482e5478018d9c4b53577c1eba9/boto/utils.py) | `f2afc6d4a441403115907cc28a45d79bac35f47d97f8cf8ebefd51ab3dc4b0d6` |
| [AWS official legacy archive](https://github.com/awsdocs/amazon-s3-developer-guide/blob/e5e6bbd2771d39e8ce3736849548a94b98d5094b/doc_source/RESTAuthentication.md) | `476586b232b4b7db11ed2329e6ccad4d6355c0bf0f3ede047d42d991914ba3d1` |

Google explicitly documents GOOG1 and AWS followed by key access ID, a colon
and a V2 signature. Its current migration guide emphasizes V4; the legacy
mechanics are additionally established by Google's pinned gsutil dependency.
GSConnection selects the HmacAuthV1Handler capability. That handler explicitly
disables HMAC-SHA256 and signs with SHA1 over the literal UTF-8 secret and UTF-8
canonical request, then base64-encodes the result. Google provider fields select
GOOG1, GoogleAccessId and x-goog extension names. No base64 secret decoding,
GOOG4/AWS4 derivation or fallback after an RSA failure is used for V2.

The fixed .NET source was read rather than inferred from current reference
summaries. Its project declares Google.Cloud.Storage.V1 5.0.0 at revision
`7db3d26977d60289be1a57fbaf3e7c3e26424e24`:

| Fixed .NET source | SHA-256 |
| --- | --- |
| [UrlSigner.cs](https://github.com/googleapis/google-cloud-dotnet/blob/7db3d26977d60289be1a57fbaf3e7c3e26424e24/apis/Google.Cloud.Storage.V1/Google.Cloud.Storage.V1/UrlSigner.cs) | `881b57e6a1eaefa60f13c65863c44f70a0391f69273f80a23e906ccf6e19d271` |
| [HmacBlobSigner](https://github.com/googleapis/google-cloud-dotnet/blob/7db3d26977d60289be1a57fbaf3e7c3e26424e24/apis/Google.Cloud.Storage.V1/Google.Cloud.Storage.V1/UrlSigner.HmacBlobSigner.cs) | `2c87a548887c98be7204c703df2fdf85aa4916ac321bd10a560cf5c5c364f517` |
| [V2Signer](https://github.com/googleapis/google-cloud-dotnet/blob/7db3d26977d60289be1a57fbaf3e7c3e26424e24/apis/Google.Cloud.Storage.V1/Google.Cloud.Storage.V1/UrlSigner.V2Signer.cs) | `b3bfc572e7df02e996819dc3e884b8ce1fd4a587d5b0ba50cedbd4bf67e54b66` |
| [V2 tests](https://github.com/googleapis/google-cloud-dotnet/blob/7db3d26977d60289be1a57fbaf3e7c3e26424e24/apis/Google.Cloud.Storage.V1/Google.Cloud.Storage.V1.Tests/UrlSignerTest.V2SignerTest.cs) | `7d89ab5f32cabf5547ddb0044d47e0f23e4da0757af25cbe8ac21164f1454bdb` |
| [Project version](https://github.com/googleapis/google-cloud-dotnet/blob/7db3d26977d60289be1a57fbaf3e7c3e26424e24/apis/Google.Cloud.Storage.V1/Google.Cloud.Storage.V1/Google.Cloud.Storage.V1.csproj) | `4cd2619161f655af78f839ab514e1ee3ae558a30bc2cafd10a4a3fb30abe2712` |

That UrlSigner defaults to V4. Its HmacBlobSigner performs GOOG4 four-stage
SHA256 derivation; forcing V2 still invokes the same blob signer. No positive
HMAC/V2 test or explicit rejection guard was established from the inspected
tests. This combination is not proof of a usable V2 HMAC URL, and dates/scopes
absent from V2 URLs are not guessed to accept it. The SDK combination remains an
explicit compatibility question. The fixed V2 signer does establish that
virtual-host object URLs sign the bucket-prefixed resource, independently of
their object-only URL path; bucket-bound custom hostnames are rejected there.

## Request and authorization contract

Three private canonical profiles separate Google query, Google header and AWS
header behavior. Query credentials retain exact GoogleAccessId/Expires/Signature
fields, duplicate/mixed rejection, bounded canonical base64 and strict existing
expiry at the injected clock boundary. A trusted RSA public-key mapping selects
RSA once; other access IDs select ACTIVE HMAC once. Header credentials require a
single GOOG1 or AWS Authorization value, nonempty key ID and canonical base64
signature, plus Date or the matching provider date extension. Opposite provider
extensions, duplicate dates/Authorization and mixed query/header credentials
are rejected.

The AWS legacy primary specifies a 15-minute header clock window. Both endpoints
of that local inclusive window are tested; V4's seven-day query or active-time
rules are not copied into V2. A provider date extension takes precedence and
empties the Date position in the signed string while remaining in canonical
extension headers. HTTP-date parsing reuses httpdate. Only parsing normalizes
the published UTC +0000 suffix to GMT: signing retains the original Date bytes.
Exact Google legacy clock/error parity remains a provider question.

| Input | Google query | GOOG1 / AWS header |
| --- | --- | --- |
| Date position | Expires Unix seconds | Raw Date, or empty when the provider date extension is present |
| Extensions | x-goog fields under the existing documented query normalization | Matching x-goog/x-amz fields; trim outer whitespace, retain internal whitespace and duplicate wire order |
| Response overrides | Not signed, as Google explicitly documents | Signed subresources under the pinned legacy canonical rules |
| Subresource values | Existing query percent-encoding choice retained | Decoded values, as pinned Boto canonical_string specifies |
| Virtual resource | Trusted bucket prefix plus literal encoded object path | Same trusted bucket prefix and literal path |

Legacy header query decoding uses percent decoding with literal `+` retained,
matching pinned Boto's `unquote`, rather than form decoding. The same bounded
decoder supplies canonicalization and XML operation/response values. Literal
`image/svg+xml` and encoded `image%2Fsvg%2Bxml` therefore produce the identical
signed and returned Content-Type; `%20` remains a distinct space. Decoded-name
duplicates, malformed percent/UTF-8 and the 16 KiB limit remain enforced.
Google query and unrelated protocols retain their existing form decoding.

V2 does not cryptographically bind a path-style Host or a port. Virtual routing
selects the bucket from the configured final host before canonicalization; the
raw Host and URI are never rewritten after signing. The typed PathStyle versus
VirtualHosted context carries that trusted bucket even when later operation
syntax fails. A different virtual bucket invalidates the signature; changing
only the port does not add an invented V2 binding.

Generation is not a supported V2 canonical subresource. Both query and legacy
header generation selection fail closed. Header generation is rejected before
key lookup or signature verification, preventing an unchanged capability from
being retargeted to private historical generations for GET, HEAD or DELETE.

The existing semaphore-admitted supervised crypto job, fresh ACTIVE lookup,
registered principal and short catalog transactions are reused. INACTIVE,
DELETED, unknown or unregistered keys fail. Key project or key-admin privilege
does not grant storage access. IAM, ACL, PAP, UBLA and creator-versus-overwrite
checks use shared authorization and atomic publication. Lookup failures keep a
typed catalog cause and XML 503; failed signatures never become anonymous under
public grants. JSON does not accept these XML signature credentials.

After successful AWS legacy authentication, only metadata, date and project-id
aliases are translated to implemented Google adapter fields. Raw opposing
prefixes and collisions are rejected, and duplicate value order is preserved.
ACL, encryption and storage-class aliases are not silently enabled by a broad
prefix rewrite. Ordinary V4, POST policy and signed-chunk canonicalization stay
separate. Explicit permissive behavior is preserved without strict fallback.

## Independent vectors and dependency boundary

The AWS archive's header GET vector is internally consistent: literal
`GET\n\n\nTue, 27 Mar 2007 19:36:42 +0000\n/awsexamplebucket1/photos/puppy.jpg`
verifies its published `qgk2+6Sv9/oM7G3qLEjTH1a1l1g=` signature with the public
example key. This tests production canonicalization and crypto together.

The same archive's query example changed its resource to awsexamplebucket1 but
retained `NpgCjnDzrM+WFzoENXmpNDUsSn8=`, which matches the old johnsmith bucket.
This mismatch remains a negative fixture. Independent OpenSSL HMAC-SHA1 over
the documented new-bucket canonical input yields `1No4mq5ETf02z8aet9voy6gui6E=`;
that fixed corrected vector verifies and path tampering fails. Neither expected
digest was generated by the local verifier. The archive discrepancy is not
silently reported as a passing published query vector.

The official stable Rust manifest was checked as 1.99.1, dated 2026-10-15,
SHA-256 `fb152be80975c194aa5fc684b2aec3a6e14e2b855efe2faa4e5f8ae6e590f463`;
ring remains current stable non-yanked 0.17.14. With explicit approval, the
already resolved httpdate 1.0.3 was promoted to a direct dependency to avoid
writing a new HTTP-date parser. The official registry identifies it as
non-yanked, MIT OR Apache-2.0, checksum
`df3b46402a9d5adb4c86a0cf463f42e19994e3ee891101b1841f30a545cb49a9`.
The lock change adds only the application-to-httpdate edge: no resolved package,
version or checksum changed. No schema, pool, thread or durable filesystem step
was introduced. fake-gcs-server v1.56.1, revision
`896ece51e49eed0946089f3089f6343855c0d106`, remains a comparison only.

## Acceptance index

All wire test IDs are children of `xml_v2_hmac` in the `xml_v4_hmac_http` target.

| Behavior | Exact wire test |
| --- | --- |
| Disk/RAM HMAC query GET/HEAD/PUT/DELETE and encoded binary names | `v2_hmac_query_transfers_get_head_put_delete_on_disk_and_ram_without_v4_derivation` |
| Disk/RAM GOOG1/AWS headers, metadata, path/virtual-host object transfer | `goog1_and_aws_legacy_headers_transfer_metadata_and_use_virtual_bucket_resources` |
| RSA/HMAC virtual queries, literal resources, bucket tamper and unbound port | `rsa_and_hmac_v2_queries_share_the_literal_virtual_bucket_resource` |
| Key state, restart, unknown identity/project, reader/key-admin, creator-only overwrite | `legacy_queries_and_headers_recheck_key_state_restart_and_never_borrow_key_project_grants` |
| Public invalid signature, JSON rejection, unknown key, duplicate/mixed credentials, MD5, actual truncated transport EOF, old complete metadata/list/bytes/staging/restart | `legacy_signatures_keep_public_failures_closed_and_preserve_complete_old_resources` |
| Current ACL/IAM, PAP/UBLA and revoked grants | `legacy_registered_signers_use_current_acl_iam_pap_and_ubla_without_implicit_grants` |
| Bucket create/HEAD/delete, service listing and explicit AWS alias rejection | `legacy_bucket_service_operations_and_aws_allowlist_reuse_shared_permissions` |
| Both header prefixes reject unsigned generation addition for GET/HEAD/DELETE and preserve private history/current state | `legacy_headers_cannot_retarget_the_unsigned_generation_to_read_or_delete_private_history` |
| Signed header response overrides versus intentionally unsigned Google query overrides | `legacy_headers_sign_response_overrides_while_google_query_retains_documented_unsigned_overrides` |

Unit cases separately fix the published header/corrected query vectors,
injected-clock query expiry and header windows, date precedence, malformed
credential/base64/mixed inputs, profile-specific fixed canonical strings,
duplicate wire order, literal-plus/percent-plus/space decoding and typed backend
failure for all three credential forms.

## Verification and remaining work

All Cargo commands use `CARGO_INCREMENTAL=0`, and test-configuration checks run
unfiltered before target tests. Full Rust, SDK and container suites are reserved
for final integration, and no cloud or metadata-server request is made.

The final production source passed 204 selected tests: 58 library tests, 78 wire
tests and 68 authentication tests, with zero failures or ignored tests. The
wire set includes all 9 new V2 cases, 11 existing V4 HMAC cases and 7 signed
chunk cases. Commands below show their actual arguments; prepend
`CARGO_INCREMENTAL=0` to every Cargo command. Each command exited zero.

| Command | Result |
| --- | --- |
| `cargo check --locked --offline --lib --tests` | Unfiltered test-configuration check PASS before the final tests |
| `cargo test --locked --lib -- auth::signature xml:: storage::catalog::hmac_keys upload_reports_a_failed_final_write single_request_upload_failures_never_publish --test-threads=1` | 58 PASS, 0 ignored; includes typed lookup failures and shared failed-write/flush/sync preservation |
| `cargo test --locked --test xml_v4_hmac_http --test xml_post_policy_http --test xml_bucket_http --test xml_multipart_http --test xml_storage_http --test sign_url_cli -- --test-threads=1` | 78 PASS: HMAC/V2/chunks 27, POST 9, bucket 13, multipart 8, storage 14, CLI 7; 0 ignored |
| `cargo test --locked --test auth_http -- signing:: xml:: xml_put:: --test-threads=1` | 68 PASS, 0 ignored; existing RSA/V2, JWT, signed policies, strict/permissive XML and PUT boundaries |
| `cargo clippy --locked --lib --bin gcs-mimic --test xml_v4_hmac_http --test xml_storage_http --test xml_multipart_http --test xml_post_policy_http --test auth_http -- -D warnings` | PASS |
| `cargo xtask lint` | 0 production violations; 1 existing reasoned `text::stringified_error` suppression |
| `git diff --check` | PASS |

After that complete run, the existing V2 public-failure test received test-only
assertions for JSON `401 authError` and unknown access-key
`403 InvalidAccessKeyId` in all three credential forms. Production source did
not change. The unfiltered check above passed again, followed by:

| Command | Result |
| --- | --- |
| `cargo test --locked --test xml_v4_hmac_http xml_v2_hmac:: -- --test-threads=1` | All 9 V2 wire cases PASS, 0 ignored; this is a recheck of the same cases, not 9 additional unique tests |
| `cargo clippy --locked --lib --bin gcs-mimic --test xml_v4_hmac_http -- -D warnings` | PASS |

The final rustfmt check covered all edited Rust files:

```sh
rustfmt --check --edition 2024 \
  src/auth/signature.rs src/auth/signature/v2.rs \
  src/auth/signature/v2/legacy.rs src/auth/signature/v2/legacy/tests.rs \
  src/auth/signature/verify.rs src/xml/mod.rs src/xml/list.rs \
  src/xml/multipart.rs src/xml/put/streaming/tests.rs \
  src/server/routing.rs src/xml/buckets.rs tests/xml_v4_hmac_http.rs \
  tests/xml_v2_hmac/mod.rs tests/xml_storage_http.rs
```

Markdownlint-cli2 0.24.0 (markdownlint 0.42.0) checked
`docs/en/slices/xml-v2-hmac-signatures.md`,
`docs/en/slices/sdk-active-storage-v2-signing.md` and `docs/en/signed-urls.md`:
3 files, 0 issues. The installed CLI was invoked through Node; its environment
path is intentionally omitted. Repository path-leak scanning of the new source,
tests and artifact found no local absolute path or username.

Initial failures were not counted as passes. The first query fixture sent a
duplicate Content-Type through a generic JSON-oriented helper; its replacement
sends exactly the signed binary wire. The inconsistent published query digest
was reconciled independently and retained as a negative. Bucket fixtures needed
their required signed project header and a legal name without the prohibited
goog prefix; production validation was preserved. A production lint finding on
a lightweight canonical context was fixed without a suppression.
The first wider regression found an old virtual-bucket negative expecting a
route error before authentication. The trusted bucket-prefixed resource now
rejects its incorrect `/` signature with 403. The test preserves that denial and
separately checks that a correctly signed virtual bucket query still returns
the unsupported-route 400 and cannot transfer to another bucket.

Independent review discovered the unsigned-generation retargeting flaw in
source. No pre-fix wire exploit was executed or claimed. The authentication
guard and versioned-object negative tests now reject the modified requests and
preserve both old and current resources; independent review reran the corrected
generation/header/query wire cases successfully. The final profile changes use
the 204-test regression above rather than reusing that earlier checkpoint.
Review also identified literal-plus form decoding. The local legacy decoder
and matching XML application path were corrected together, with fixed Boto
canonical strings and exact response-header wire cases.
Final independent source review, including the fixed Boto digest, raw-query
helper, all changed callers and strengthened virtual-bucket negatives, reported
zero major findings.

Remaining DESIGN-040 scope includes complete POST-form and signed resumable
operations, virtual-host bucket query edge cases, strict official SDK network
coverage, the .NET forced-V2/HMAC combination, Google-exact header timing/error
behavior and ambiguous subresource/query encoding, including empty assignment
and encoded-name parity. Unimplemented AWS aliases,
user-account HMAC models and advanced operation interactions are not claimed by
these local principal tests. No new publication step was added: existing failed
write/flush/sync and signed-chunk fault regressions remain required, while full
crash/PowerLoss matrices were not rerun here. Central alone updates the feature,
gap and generated matrices; no parent scope is reduced to claim completion.
