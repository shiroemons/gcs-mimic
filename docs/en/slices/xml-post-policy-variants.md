# XML signed POST policy variants

<!-- Exact test identifiers and commands remain on single table rows. -->
<!-- markdownlint-configure-file {"MD013": {"tables": false}} -->

This slice adds V4 HMAC-SHA256 and V2 RSA-SHA256/HMAC-SHA1 policy verification
to the existing V4 RSA form upload path (DESIGN-041, the V2 POST portion of
DESIGN-040, and EXT-BEH-004). The broader POST operation and provider-exact
policy behavior remain incomplete; the limits below are not removed from the
parent feature scope.

## Contract and pins

The following primary sources were checked on 2026-10-31. Digests identify the
retrieved documents, including their surrounding HTML; they are not promises
that an unversioned documentation URL will retain the same bytes.

| Primary source | SHA-256 |
| --- | --- |
| [POST Object forms](https://docs.cloud.google.com/storage/docs/xml-api/post-object-forms) | `fc443bd728ae26ee9ecab144463a45e6414ad37c732ac531887545f51c0f95f0` |
| [V2 POST Object](https://docs.cloud.google.com/storage/docs/xml-api/post-object-v2) | `fbd52a1d7d33a9541073dd2ae707e9f2b381df6a68573da9c7bdeb36861d2919` |
| [Signatures and policy documents](https://docs.cloud.google.com/storage/docs/authentication/signatures) | `ee16ec35fe9496c78b87da5fb54c2c9950759abdf88183304cfab981829225b6` |
| [Canonical requests](https://docs.cloud.google.com/storage/docs/authentication/canonical-requests) | `d4b7e058fc9b55258a8a007ab4b9056d7a422418938a2c250a25edaf5700869e` |
| [XML headers](https://docs.cloud.google.com/storage/docs/xml-api/reference-headers) | `3c564c341dfb599332bd8f35a1acc10d8ad557caf1af3f53b95c672da2330e9c` |
| [XML status codes](https://docs.cloud.google.com/storage/docs/xml-api/reference-status) | `d30c6c5a06ae9064bc5a34ae2fc486c4b98f408494dac4fbda20650f85dc9053` |
| [RFC 2202 HMAC vectors](https://www.rfc-editor.org/rfc/rfc2202.txt) | `c19effeca47e801be304460da3b2fb05f595e2309abceb050e40e024868fe483` |

The official stable Rust manifest was verified as Rust 1.99.1, dated
2026-10-15, digest
`fb152be80975c194aa5fc684b2aec3a6e14e2b855efe2faa4e5f8ae6e590f463`.
The official registry's current stable ring release was 0.17.14, non-yanked,
checksum `a4689e6c2294d81e88dc6261c768b63bc4fcdb852be6d1352498b114f61383b7`.
No dependency, schema, durable publication step or additional worker pool was
introduced. fake-gcs-server v1.56.1, revision
`896ece51e49eed0946089f3089f6343855c0d106`, remains a comparison implementation.

## Request, authentication and mutation

| Version | Credential fields | Signature input and algorithm | Condition exemptions |
| --- | --- | --- | --- |
| V4 RSA | `x-goog-algorithm`, `x-goog-credential`, `x-goog-date`, `x-goog-signature` | Exact submitted base64 `policy` bytes; RSA-SHA256, hex signature | `policy`, `x-goog-signature`, file part |
| V4 HMAC | Same fields, algorithm `GOOG4-HMAC-SHA256` | Exact base64 policy bytes; four scope-derived HMAC-SHA256 rounds, hex signature | Same as V4 RSA |
| V2 RSA | `GoogleAccessId`, `policy`, `signature` | Exact base64 policy bytes; RSA-SHA256, base64 signature | `GoogleAccessId`, `policy`, `signature`, file part |
| V2 HMAC | Same V2 fields, access ID as `GoogleAccessId` | Exact base64 policy bytes; literal-secret HMAC-SHA1, base64 signature | Same as V2 RSA |

Field names remain case-insensitive and duplicate fields are rejected. Mixed
V2/V4 fields, partial credential sets, credentials without a policy and
policy-plus-Authorization requests fail closed. The Google V4 algorithm names
above are supported; AWS form aliases have no established Google contract here.
The V2 RSA key mapping is selected before verification; verification failure
never retries with HMAC. V2 HMAC-SHA1 is confined to this documented legacy
form-policy job and is not a V4 authentication option.

The existing fresh ACTIVE HMAC lookup resolves the currently registered service
account. INACTIVE, DELETED, unknown and unregistered keys fail authentication.
The key's project and a key-administrator role do not grant object access. The
resolved principal goes through the shared IAM/ACL/UBLA/PAP authorization and
atomic create/overwrite checks on the actual request bucket. Valid signed
principals remain usable under PAP. Revocation affects newly authenticated
requests; this slice does not claim cancellation of requests already admitted.
Catalog failures retain a typed source and become 503, rather than unknown-key
or anonymous success. Secrets are not cached or printed.

RSA and HMAC verification reuse the semaphore-admitted, supervised blocking job
used by XML query/header signatures. The job owns its permit through completion;
no cryptographic computation is added to a catalog transaction or async worker.
Caller lifetimes and the existing HMAC/query/header APIs are preserved.

V4 requires a bucket condition. V2 explicitly permits omitting it: such a
policy does not bind the target bucket, while shared authorization still does.
V2 checks the policy's expiration without importing V4 date/scope fields or
signed-URL timing rules. The existing V4 local 900-second early allowance and
seven-day maximum remain visible deviations: neither was established as a
form-policy requirement. The checked official Go storage v1.70.0
`post_policy_v4.go` validates a future expiration without a seven-day ceiling.

The bounded multipart parser and upload path are reused: at most 64 fields,
64 KiB per non-file field, 256 KiB aggregate fields, one final file part, bounded
body chunks and checked stream EOF before publication. Policy ranges are
inclusive. Rejected conditions, insufficient overwrite permissions and failed
write/flush/sync leave old bytes, metadata, generation and object count intact.
No-policy requests without Authorization use anonymous permissions. No-policy
Authorization is an existing local extension, not the documented anonymous
Google form contract.

Success retains the existing 204/200, 201 XML and local 303 redirect behavior.
The redirect Location is preserved exactly. Google status/query augmentation
details remain unverified; no S3 redirect convention was inferred.

## Acceptance index

All test IDs below are exact Rust test names. Results are scoped to their
assertions and do not complete the pending parent cases listed afterward.

| Behavior | Tests |
| --- | --- |
| V4 RSA/HMAC binary upload on disk and RAM | `v4_rsa_and_hmac_policy_upload_exact_binary_on_disk_and_ram` |
| V4 HMAC ACTIVE state, restart, revocation, unknown principal, key-admin/project isolation | `v4_hmac_policy_keys_revoke_restart_and_never_borrow_key_admin_or_project_permissions` |
| V4 tampering, malformed/duplicate fields, public invalid-signature denial, old complete resource | `v4_hmac_policy_rejections_preserve_the_complete_old_resource_even_when_public` |
| V2 RSA encoding, optional bucket coverage, independent expiration and rejected writes | `v2_rsa_policy_uses_base64_signature_and_its_own_bucket_coverage_and_expiration` |
| V2 literal-secret SHA1, key states/restart, principal isolation and old resource | `v2_hmac_policy_uses_active_literal_secret_sha1_and_preserves_rejected_overwrites` |
| All four variants, exact minimum/maximum, one byte outside each limit, creator-only new/overwrite | `every_policy_variant_checks_inclusive_ranges_and_creator_only_overwrites` |
| All four variants, 201 XML and existing unchanged redirect Location | `every_policy_variant_preserves_created_xml_and_redirect_responses` |
| All four variants, scoped IAM grant/revocation under PAP and UBLA | `every_policy_variant_rechecks_scoped_iam_under_pap_and_ubla` |
| Mixed header/version denial, explicit anonymous and local no-policy Authorization paths | `policy_credentials_never_mix_with_headers_or_partial_versions_and_anonymous_extension_is_explicit` |
| Four variants, initial/tail write, flush and sync faults; old record/bytes/count, staging cleanup and restart | `xml::post::failure_tests::policy_variants_keep_old_objects_after_write_flush_and_sync_failures` |
| Both HMAC variants, closed catalog typed cause and 503 without fallback | `xml::post::failure_tests::hmac_policy_backend_failure_retains_cause_and_never_returns_anonymous` |
| Independent published SHA1 digest, with negative V4 derivation comparison | `auth::signature::verify::tests::legacy_policy_hmac_sha1_matches_rfc2202_without_v4_derivation` |

## Verification

Commands use Rust 1.99.1 and `CARGO_INCREMENTAL=0`. Unfiltered compilation passed
before the final targeted test runs. No ignored tests or increased test stacks
are used. Local server tests require loopback socket access.

| Command | Result |
| --- | --- |
| `cargo check --locked --lib --tests` | PASS, including test-only fault helpers |
| `cargo test --locked --lib -- xml::post:: auth::signature::verify::tests::legacy_policy_hmac_sha1_matches_rfc2202_without_v4_derivation --test-threads=1` | PASS: 12 tests |
| `cargo test --locked --test xml_post_policy_http --test xml_v4_hmac_http --test xml_bucket_http --test xml_multipart_http --test xml_storage_http --test sign_url_cli -- --test-threads=1` | PASS: 9 + 11 + 13 + 8 + 14 + 7 = 62 tests |
| `cargo test --locked --test auth_http -- signing:: xml:: xml_put:: --test-threads=1` | PASS: 68 tests; signing/JWT/policy 33, XML 22, XML PUT/hold 13 |
| `cargo test --locked --lib -- auth::signature xml:: storage::catalog::hmac_keys upload_reports_a_failed_final_write single_request_upload_failures_never_publish --test-threads=1` | PASS: 48 tests |
| `cargo clippy --locked --lib --bin gcs-mimic --test xml_post_policy_http --test xml_v4_hmac_http --test auth_http -- -D warnings` | PASS |
| `rustfmt --edition 2024 --check` on the changed Rust files | PASS |
| `cargo xtask lint` | PASS: zero production violations, one existing reasoned suppression |
| Markdown lint 0.24.0 / markdownlint 0.42.0 on this slice, signed-urls.md and xml-v4-signatures.md | PASS: three files, zero issues |

Initial failures were not counted as passes. A fixture type inference error and
test-only private fault import prevented compilation and were corrected with an
explicit field map type and test-only reexport. An early test was inadvertently
queued before its failed check result was inspected; check and tests were then
rerun in the required order. A restart assertion exposed fixture metadata URLs
changing with ephemeral ports; pinning the advertised fixture origin preserved
the full metadata comparison. A new redirect expectation incorrectly assumed
bucket/key/etag query augmentation without Google evidence; it was corrected
to verify the existing unchanged Location, with provider behavior still pending.
No existing negative assertion or production redirect validation was weakened.
Final Clippy also found one unnecessary fixture clone, removed before its
successful rerun. Markdown lint required the table-only line-length exception
above for exact commands and identifiers; prose remains checked at 80 columns.
Independent review of the final source, callers, fixtures, denials and gates
found no blocking or major issue. Its acceptance-index wording correction was
applied without changing test assertions or narrowing the pending scope.

## Remaining scope

- Service-root V2 POST with bucket encoded in `key`, form/URI/virtual-host target
  resolution and their mismatches remain pending under POST-FORM-TARGET-ROUTING.
- The current validator accepts repeated conditions for one field and V4 exact
  or starts-with Content-Length conditions, despite the V4 documentation's
  separate-form advice and range-only Content-Length rule. Policy base64
  decoding also does not reject nonzero unused padding bits. These are concrete
  existing validator follow-ups, not completed acceptance cases.
- Filename expansion currently occurs after conditions match the submitted key.
  Google evaluation order, path/Unicode/Windows basename behavior and Content-Type
  comma-list behavior remain unresolved; no S3 normalization rule was imported.
- V4 form timing limits, AWS form aliases, provider-exact errors and redirect
  status/query rules remain pending contract questions. V2 optional bucket
  conditions are a documented security boundary, not an authorization bypass.
- Multipart field variants, interrupted network bodies, early cancellation and
  strict official SDK form interoperability still need their complete declared
  acceptance evidence. Existing parser/unit/negative regressions do not imply
  each untested four-variant combination has passed.
- XML resumable initiation, signed streaming/trailer uploads and other XML
  operation parents remain separate pending work. Authorization-signed Google
  chunked uploads belong to DESIGN-039 itself, not an excluded S3-only scope.
- No full SDK suite, container build, real-cloud differential, full Rust suite or
  crash/PowerLoss recovery matrix was run for this slice. No new durable step was
  added; existing failure seams were exercised directly through POST, while
  final integration remains coordinated separately.

Central feature/gap/matrix changes are owned by the compatibility maintainer.
DESIGN-041 and its operation/authentication parents must retain all broader
pending cases above rather than being marked complete from these variant tests.
