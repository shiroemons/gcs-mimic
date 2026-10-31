# XML bucket object listing

<!-- markdownlint-configure-file {"MD013": false} -->

Contract checked **2026-10-30** for EXT-XML-003 and EXT-BEH-001.
The feature ledger owns status. This is a scoped implementation; owner information
and full provider error parity remain unsupported.

## Sources

- [Google XML List objects](https://docs.cloud.google.com/storage/docs/xml-api/get-bucket-list), last updated 2026-10-26; retrieved HTML SHA-256 `ae04b14dd55032c1eaa9fd596af1924a258815391e24e6a9cf50ecd5454b9fab`.
- [Google XML headers and parameters](https://docs.cloud.google.com/storage/docs/xml-api/reference-headers), checked 2026-10-30, specifies list versions, marker aliases, positive `max-keys` and URL encoding.
- [Pinned fake-gcs-server object handler](https://github.com/fsouza/fake-gcs-server/blob/896ece51e49eed0946089f3089f6343855c0d106/fakestorage/object.go#L695-L748), v1.56.1, source SHA-256 `cfd884eb03aa429f6b681bef8ca1923c600b24beb850c0620121c0c23110a56b`, is comparison evidence only. Its XML handler reads prefix, delimiter and `versions=true`; it does not establish Google's pagination contract.

Discovery/protobuf do not define this XML wire operation. Existing shared JSON
and gRPC listing records keep their own pinned contracts.

## Supported request and response

`GET /BUCKET` and `GET /BUCKET/` list objects. The configured virtual-host XML
endpoint also lists at `/`; bucket selection and signatures keep the original
Host/path. JSON and internal namespaces remain isolated by existing routing.
V2 query signatures remain path-style only: bucket-root virtual-host requests
are rejected before dispatch, preventing a signature from selecting another
bucket through a changed Host. V2 signs the `versions` subresource and its value;
adding, removing or changing it invalidates the signature. Ordinary list filters
remain unsigned under Google's [V2 canonical-resource rules](https://docs.cloud.google.com/storage/docs/access-control/signed-urls-v2).

- Omitted `list-type` and `list-type=1` select v1. `marker`/`key-marker` are exclusive object-name lower bounds. `generation-marker`/`version-id-marker` require a nonempty name marker and `versions` and resume after that generation.
- `list-type=2` uses `start-after` or an opaque `continuation-token`. A continuation token takes precedence over `start-after`. V1-only markers are rejected on v2, and v2-only parameters are rejected on v1.
- `prefix` and a possibly multi-character `delimiter` reuse shared indexed seeks, deduplicate common prefixes and include both object and prefix entries in the page bound. A marker inside a grouped prefix skips that entire prefix.
- `max-keys` accepts a positive uint32 and defaults to 1000. Responses echo the requested maximum; the local catalog returns at most 1000 combined entries even when more were requested. Clients must follow `IsTruncated`.
- Presence of `versions` selects retained and current generations in name/generation order. The legacy representation uses `Version`, `Generation`, `IsLatest`, `DeletedTime`, `NextMarker` and `NextGenerationMarker`. With `x-goog-interop-list-objects-format: enabled`, the root is `ListVersionsResult` and generation/marker fields use `VersionId`, `KeyMarker` and `VersionIdMarker` spellings.
- `encoding-type=url` percent-encodes UTF-8 names and name-bearing fields once, including slash, percent and non-ASCII bytes. Without it, ordinary XML entities are escaped. This option should be used for names containing characters that XML 1.0 cannot represent.
- Entries contain name, selected generation/metageneration, modification time, XML ETag, size and storage class. ETags use the same implementation as XML GET/HEAD; customer-key and composite ETags retain their existing opaque forms.
- V2 responses include `KeyCount`, optional request-token/start-after echoes and a next token only for a truncated page. Empty/exhausted pages are valid successful responses without a next token.

Continuation tokens add an XML-v2 discriminator to the existing bounded,
checksummed cursor codec and bind bucket, prefix, delimiter and version selection.
They are pagination state, not authorization credentials. Every request repeats
strict `storage.objects.list` authorization, including the catalog's policy
check; changing IAM immediately affects subsequent requests.

Known unsupported parameters, owner requests, object preconditions/encryption on
bucket listing, duplicate query names, malformed tokens and conflicting marker
spellings fail explicitly. `fetch-owner=false` is supported on v2;
`fetch-owner=true` returns 400 `InvalidArgument` because uploader ownership is not
modeled. Unsupported valid provider semantics follow the existing local fail-fast
policy. Exact provider error precedence/details and token encoding are not claimed.
Successful listing changes no catalog state or object data.

## Bounds and acceptance

The adapter collects one catalog page of at most 1000 entries and its XML
representation. It never loads the whole catalog or object bodies. It adds no
dependency, SQL implementation, durable write path, staging file or schema.
Existing connection/header/deadline/catalog-admission limits apply. Bodyful GET
requests are rejected through the bounded zero-byte body reader.

`cargo check` and `cargo check --test xml_storage_http` passed. With local HTTP
listeners permitted, `cargo test --test xml_storage_http` passed **13 tests**
across LIST and COPY; the six LIST cases are:

| Test | Verified behavior |
| --- | --- |
| `xml_bucket_list_v1_v2_pages_prefixes_markers_and_empty_results` | Single-entry v1/v2 pages, colliding object/prefix names, delimiter group skipping, start-after, empty/exhausted responses |
| `xml_bucket_list_encodes_once_and_keeps_object_namespace_collisions` | XML entity escaping, UTF-8/percent/slash encoding and JSON/internal-looking object names |
| `xml_bucket_list_versions_resume_by_generation_and_survive_restart` | Retained generations, generation-marker continuation, latest flag, restart and interoperable version representation |
| `xml_bucket_list_rejects_invalid_scopes_options_and_preserves_objects` | Invalid options, duplicate fields, malformed/cross-bucket/filter/version cursors, XML-versus-JSON token discrimination, unchanged generation/bytes and missing-bucket XML errors |
| `xml_bucket_list_enforces_strict_list_permission_and_revocation` | Anonymous/creator/roleless denials, bucket-scoped viewer grant and revocation |
| `xml_bucket_list_v2_signatures_reject_host_transfer_and_bind_versions` | Strict V2 path-style success, signed versions success, hostile virtual Host rejection and versions addition/removal/value tampering |

The V2 signature unit filter passed three tests, including
`v2_binds_versions_subresource_and_value_while_ordinary_list_filters_are_unsigned`.
`cargo test --test auth_http xml` passed 46 existing XML/signature/CORS cases.
`cargo xtask lint` reported zero production violations, and
`cargo fmt --all -- --check` passed.

Official Rust Arrow `object_store` SDK, container, browser and real-cloud
verification remain integration work. Passing these tests does not establish
whole-XML compatibility or complete EXT-XML-003/EXT-BEH-001 acceptance.
