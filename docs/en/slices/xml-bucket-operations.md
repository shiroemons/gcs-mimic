# XML bucket operations and object deletion

<!-- markdownlint-configure-file {"MD013": false} -->

Contract checked **2026-10-31** for EXT-XML-001/002/006/008/010.
The feature ledger owns implementation status. This contract keeps remaining
provider fields and shared-domain limitations explicit.

## Pinned sources

The operation references were updated on 2026-10-26 and the status reference on
2026-10-30; all were fetched on 2026-10-31.
Discovery and protobuf describe shared records, not these XML wire operations.
The fixed fake-gcs-server v1.56.1 revision
`896ece51e49eed0946089f3089f6343855c0d106` is comparison material only.

| Reference | Retrieved HTML SHA-256 |
| --- | --- |
| [GET Service](https://docs.cloud.google.com/storage/docs/xml-api/get-service) | `94954ced890f4ee5426a3fc70c7db5891b43a6cf60ccadab3ecb46595e57c94a` |
| [HEAD Bucket](https://docs.cloud.google.com/storage/docs/xml-api/head-bucket) | `54218cbc9997906ecf031193f99c1e176221e59ce9144e1b90dd18d5c9e3b0ef` |
| [PUT Bucket](https://docs.cloud.google.com/storage/docs/xml-api/put-bucket-create) | `1f61276afd38fadc385c98d2940bb6ff91169a5d14cad7e58cd98dd187b79949` |
| [DELETE Bucket](https://docs.cloud.google.com/storage/docs/xml-api/delete-bucket) | `32e4a505d68a527065ebab98184c5975b5ae592c46ab1ece029763a51e970160` |
| [DELETE Object](https://docs.cloud.google.com/storage/docs/xml-api/delete-object) | `c9a71034588ae77badac2f5f12f6e50f0d5f6dfe13c51bdc6ab2292cc822da86` |
| [XML status codes](https://docs.cloud.google.com/storage/docs/xml-api/reference-status) | `d30c6c5a06ae9064bc5a34ae2fc486c4b98f408494dac4fbda20650f85dc9053` |

Official Rust stable manifest dated 2026-10-15 identifies Rust 1.99.1,
commit `58cf8f9c3`; fetched SHA-256
`fb152be80975c194aa5fc684b2aec3a6e14e2b855efe2faa4e5f8ae6e590f463`.
Metadata parsing adds **quick-xml 0.42.0**, with default features disabled.
The [official Cargo index](https://index.crates.io/qu/ic/quick-xml) verifies the
non-yanked stable release, Rust 1.86 minimum and crate SHA-256
`41b1177fdf999d2321d3fb46ff47159d9c1fb9ad66a4879f8c50a0b504615e9b`.
The 233,019-byte archive ships `LICENSE-MIT.md`; the maintained publisher's
2026-08-22 release retains security fixes. Its only nonoptional dependency is
the already resolved `memchr`; no async, serde or alternative encoding feature
is enabled. Package size does not measure executable growth. Cargo generated
the lockfile. The dependency snapshot records the complete freshness check.

## GET Service

`GET /` selects the service only when bucket-host selection did not select a
bucket. `x-goog-project-id` selects the project; its identifier is validated and
strict callers need `storage.buckets.list` on that project. Permissive mode keeps
its explicit authorization profile. There is no configured interoperable-account
default project: omission fails instead of guessing from fixture placement.

The 200 XML document has `ListAllMyBucketsResult`, `Buckets`, and a `Bucket`
containing `Name` and persisted `CreationDate` per entry. Names are in lexical
order and limited to the first **1000**, as Google's reference specifies.
The XML reference defines no continuation token; use the JSON bucket list for
larger projects. Empty projects return an empty `Buckets` element. The adapter
walks bounded catalog pages, including metadata-budget-shortened pages, while
retaining only the bounded XML output and current page. It reads no object body.

Signing uses the unchanged URI and signed project header. Unsupported options,
requester billing, object conditions/encryption/ranges, duplicate query/header
values, and request bodies fail explicitly. Unknown Google semantics retain the
local nonretryable rejection policy under SPEC-SDK-UNSUPPORTED. Exact Google
authentication wording and precedence are not claimed.

## HEAD Bucket

`HEAD /BUCKET`, `HEAD /BUCKET/` and configured bucket-host `HEAD /` require
`storage.buckets.get` through the shared bucket authorization and authorized
catalog lookup. A missing accessible bucket returns 404. Success is 200 with
no body; errors also transmit no body. Path-style bucket lookup decodes once,
and signature validation preserves raw encoding and Host.

Reserved JSON/internal namespaces retain their existing routing. V2 bucket-host
requests remain explicitly unsupported because the local V2 verifier does not
bind Host. V4 signs Host and raw path. Unsupported bucket subresources, object
generation conditions/encryption, requester billing and request bodies are
rejected; this endpoint changes no state.

## PUT Bucket

`PUT /BUCKET` or configured bucket-host `PUT /` creates a bucket. The adapter
requires a validated project header and strict `storage.buckets.create` permission
before reading metadata. Empty bodies use US and STANDARD, independently of the
JSON fixture/default-location configuration. `CreateBucketConfiguration` accepts
`LocationConstraint` and `StorageClass` in either order. Shared bucket parsing
validates the names, location and class before the existing atomic insertion.
Success returns 200 without a body; conflicting names return 409
`BucketNameUnavailable`. Invalid names and inputs do not publish a bucket.

`x-goog-bucket-object-lock-enabled` accepts true/false. True additionally requires
`storage.buckets.enableObjectRetention` and persists the shared object-retention
enablement. `x-goog-bucket-retention-period` uses shared duration validation and
enforces the resulting policy on object deletion. Supplied Content-MD5 and
SHA256 are checked against the complete bounded body before insertion; a bad
digest never creates the bucket. V4 signing retains the raw URI and headers.

The request body is limited to 64 KiB. The metadata reader additionally limits
depth to 8, elements to 256, attributes per element to 16 and namespace bindings
in scope to 128. It accepts XML 1.0 declarations with legal whitespace/quotes,
optional UTF-8 encoding/standalone, recognized default or prefixed namespaces,
comments, CDATA and standard/numeric references. It rejects DTDs, custom entities,
illegal XML characters, mismatched/unclosed tags, duplicate attributes, multiple
roots, trailing content and invalid declarations. The creation adapter separately
validates the root, scalar fields and duplicates. It does not confuse syntactic
XML validity with supported bucket settings.

Custom placement, encryption configuration, predefined ACLs, requester billing
and other unsupported settings fail explicitly. The legacy compatibility profile
supports default creation but rejects location/class configuration rather than
silently ignoring it. These limits do not narrow the parent operation's scope.

## DELETE Bucket and DELETE Object

`DELETE /BUCKET` and bucket-host `DELETE /` use shared
`storage.buckets.delete` authorization and the catalog's atomic emptiness check.
Success is bodyless 204; missing buckets are `NoSuchBucket`/404 and live or
retained noncurrent objects produce `BucketNotEmpty`/409. Bucket/object metadata
and deletion survive restart. The existing purge of soft-deleted objects on
bucket deletion remains the documented SPEC-SOFT-DELETE difference: Google
preserves that state. No new durable write, rename or sync step is introduced.

`DELETE /BUCKET/OBJECT` retains selected `generation` and the two XML
generation/metageneration match headers. The shared mutation checks conditions
and current authorization atomically. V4 signed queries now reach that mutation;
the signing helper and CLI accept DELETE while retaining their POST rejection.
Changing method, Host, raw path or selected generation invalidates the signature.
Success is bodyless 204; a missing live object is `NoSuchKey`/404 and stale
conditions return `PreconditionFailed`/412. Holds return
`ObjectUnderActiveHold`/403; unexpired retention returns
`RetentionPolicyNotMet`/403. Versioning, soft-delete/restore and PAP/UBLA use the
shared state transitions and policy, including explicit signer access under PAP.

## Verification

`cargo check --locked`, `cargo check --locked --test xml_bucket_http` and
`cargo test --locked --test xml_bucket_http` (**13 passed**) passed.
The final shared-reader reuse also passed `cargo test --locked --lib xml::`
(20 tests), the signing CLI (7 tests) and existing XML storage wire corpus
(14 tests). Earlier targeted regressions passed all 33 signing and 22 XML
authorization/negative cases plus 14 signature units. These are scoped native
checks, not a whole-repository or SDK/container result. The existing XML PUT
regression target also passed all 13 cases. Its hold case expects the official
403 `ObjectUnderActiveHold` while preserving rejection, complete metadata/bytes/
generation/count snapshots and their equality after restart.

Targeted strict Clippy passed for the API/parser candidate. The final borrow-only
namespace cleanup passed type checking; its unit/production-lint rerun could not
compile the concurrently edited `src/storage/catalog/upload_replay.rs` because
of E0282/E0283 error-type inference failures. The final shared-tree production
gate is not recorded as passing; integration owns its completion.

| Wire test | Acceptance |
| --- | --- |
| `xml_service_lists_only_selected_project_in_order_with_persisted_creation_dates` | Project isolation, order, XML fields/type/namespace, empty project, persisted creation dates after restart |
| `xml_service_bounds_and_validates_project_headers_query_and_body` | Required/empty/malformed/duplicate project, unknown/duplicate query, requester billing, method override, body rejection |
| `xml_service_strict_authorization_and_signed_project_binding` | Admin success, anonymous/object-viewer/roleless denials, V4 project-header binding |
| `xml_head_bucket_checks_access_and_sends_no_body_on_success_or_error` | Path/trailing slash/virtual host, one decode, missing bucket, permission denials, raw signed path, bodyless success/errors, reserved internal route |
| `xml_service_returns_the_official_first_thousand_bucket_limit` | 1000/1001 boundary, lexical first 1000, bounded multi-page traversal |
| `xml_bucket_create_delete_defaults_fields_conflicts_nonempty_and_restart` | XML creation defaults, one decode, location/class, duplicate/nonempty/missing errors, bodyless deletion, restart |
| `xml_bucket_create_rejects_invalid_bounded_metadata_without_creating` | Malformed XML/declarations/root, invalid names/locations/classes/headers/durations, unsupported fields/query, oversized body and no publication |
| `xml_bucket_strict_creation_retention_signed_body_and_deletion_permissions` | Project creation/delete permissions, virtual-host creation, retention enablement/policy effects, V4 signed SHA256 mismatch and success, signed bucket deletion |
| `xml_delete_object_enforces_generation_metageneration_versioning_and_signed_binding` | Selected generations, stale match conditions, delete denials, signature method/Host/raw-path/query binding, noncurrent generations, missing-object XML and restart |
| `xml_head_bucket_uses_iam_ubla_and_public_access_prevention` | Bucket-scoped and public grants, UBLA/PAP interaction, explicit signer access and project-list isolation |
| `xml_delete_object_preserves_soft_deleted_bytes_and_enforces_holds_and_pap` | Active-hold denial, soft deletion, PAP/UBLA denial of public grants, signed deletion, restart and exact-byte restore |
| `xml_bucket_create_verifies_md5_and_separates_retention_enable_permission` | Correct/mismatched/malformed MD5, no publication on failure, create-only editor grant denied retention enablement |
| `xml_bucket_create_accepts_standard_xml_declarations_namespaces_entities_and_cdata` | Legal declaration/namespace/entity/comment/CDATA forms, malformed declarations, unknown namespaces, duplicate xmlns, multiple roots, trailing content and DTD rejection |

## Remaining work

Custom placement and KMS/encryption enforcement currently lack shared bucket
fields and effects (`BucketRecord` and JSON unsupported fields). Predefined ACLs
and broader bucket ACL authorization require their own shared implementation.
No XML adapter accepts these as metadata-only success.

Bucket soft deletion/restoration, preservation of soft-deleted objects after
bucket deletion and complete incomplete-upload interaction evidence remain
pending. V2 virtual-host/generation limitations remain explicit. The
[multipart completion parser](xml-multipart-upload.md) reuses the pull reader
with its existing 2 MiB/10000-part contract; other XML subresources remain
separate work.

Other XML subresources, IAM/ACL effects not implemented by the shared domain,
interoperable account defaults, requester billing, HMAC/header authentication,
provider-exact errors, cloud differential runs, SDK/container integration and
physical power-loss verification remain outside the passing evidence above.
