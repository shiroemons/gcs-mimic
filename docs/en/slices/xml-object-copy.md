# XML object copy

<!-- markdownlint-configure-file {"MD013": false} -->

Contract checked **2026-10-30** for EXT-XML-024. The feature ledger owns status.
This slice implements a bounded copy subset through the existing shared engine;
it does not claim complete XML copy compatibility.

## Sources and supported request

[Google XML Copy an object](https://docs.cloud.google.com/storage/docs/xml-api/put-object-copy),
last updated 2026-10-26, retrieved HTML SHA-256
`b927bc4a965b21e6cd85364bf694c2110e8326739db07d05376ff4368e82e9f8`,
defines PUT with a source header, metadata directives and source conditions.
The [XML headers reference](https://docs.cloud.google.com/storage/docs/xml-api/reference-headers)
defines the separate source generation and requires a source generation selection
or match condition when using a source metageneration condition.
These XML contracts are independent of JSON Discovery/protobuf inventories.
The comparison baseline remains fake-gcs-server v1.56.1, commit
`896ece51e49eed0946089f3089f6343855c0d106`; no differential execution is claimed.

`PUT /DESTINATION_BUCKET/OBJECT` with `x-goog-copy-source: /SOURCE_BUCKET/OBJECT`
copies a source object. The leading source slash is optional. Bucket and object
names are percent-decoded once; literal question marks, fragments, percent signs
and non-ASCII bytes must be encoded. The source generation is selected with
`x-goog-copy-source-generation`, rather than an embedded query string.
Existing path-style and configured virtual-host routing, bearer authentication,
V4 signing and XML CORS apply. V2 retains its path-style restrictions.

Supported conditions are:

- Source `x-goog-copy-source-if-generation-match` and `x-goog-copy-source-if-metageneration-match`, checked while pinning the selected immutable source; metageneration needs a selected or matched generation.
- Source `x-goog-copy-source-if-match` and `x-goog-copy-source-if-none-match` with one strong XML ETag, optionally quoted. These inspect the pinned source's XML ETag.
- Destination `x-goog-if-generation-match` and `x-goog-if-metageneration-match`, including generation zero for create-only. The shared publication transaction rechecks them, destination ownership, create/delete permissions, holds and retention before mutation.

The caller must have source `storage.objects.get` and destination
`storage.objects.create`, plus destination delete permission when overwriting.
The shared engine rechecks source read grants and bucket identity at publication.
Source generation/metadata conditions apply to the pinned source snapshot even
if its live name is overwritten or deleted during transfer.

## Metadata, integrity and response

Omitted `x-goog-metadata-directive` and `COPY` inherit source content type,
encoding/disposition/language, cache control, custom metadata and custom time.
Request metadata does not replace inherited values under `COPY`.
`REPLACE` uses only supplied supported metadata, with application/octet-stream
when content type is omitted. Source holds, retention and ACLs are not copied;
destination bucket defaults and existing shared ACL/publication rules apply.
An absent class inherits the source class; explicit supported
`x-goog-storage-class` selects another class under shared Autoclass rules.
Customer-supplied source and destination keys use their existing distinct header
prefixes; source keys must match before pinning, and destination key fingerprints
are recorded without storing the keys. Local disk encryption remains simulated
under the existing customer-key contract.

The copy engine streams bounded disk chunks and computes MD5/CRC32C incrementally.
By default the resulting hashes must match the pinned source metadata.
Optional `Content-MD5` and `x-goog-hash` verify the copied object under the local
integrity contract; malformed or conflicting declarations fail explicitly and
mismatches return `BadDigest` without changing the destination.
This checksum-header interpretation has not been differentially verified against
Google's copy endpoint.

The successful XML body is `CopyObjectResult`, containing the copied source's
`LastModified` and the destination XML `ETag`. Response headers identify the
new destination generation, metageneration, checksums and applicable encryption
or composite metadata. This preserves existing XML ETag semantics.

## Explicit limits

- Self-copy metadata updates, source date validators, ETag wildcards/lists/weak validators, copy invocation-token replay, explicit ACL/KMS/retention/custom-time request headers and other unsupported XML fields return 400 `InvalidArgument`. Use supported JSON metadata updates for self-copy. They do not silently publish a new generation with different semantics.
- Request bodies must be empty. Existing framing, header, connection, object-size, catalog-admission and request-deadline bounds apply.
- There is no new SQL, schema, staging/publish protocol or durability seam. The adapter reuses source pinning, ephemeral derived transfer, bounded advance, publication, cancellation cleanup and startup recovery. It never reads an object or whole catalog into memory.
- Official Rust Arrow `object_store`, container/browser checks and real-cloud differential verification remain integration work. Parent feature status remains PARTIAL.

## Acceptance

`tests/xml_storage_http.rs` exercises the following wire behavior:

| Test | Behavior |
| --- | --- |
| `xml_copy_selects_generation_and_copies_or_replaces_metadata_with_integrity` | Encoded source names, source generation/metageneration/ETag, inherited/replaced metadata, MD5/CRC32C, old generation selection, unchanged source and restart |
| `xml_copy_failed_conditions_digests_and_unsupported_inputs_preserve_destination` | Source/destination 412, malformed/mismatched digests, invalid/duplicate headers, unsupported inputs and complete destination metadata/bytes preservation |
| `xml_copy_create_only_condition_has_one_concurrent_winner` | Four concurrent create-only copies publish exactly one generation |
| `xml_copy_requires_source_get_destination_create_and_overwrite_delete` | Strict source read/destination create/overwrite-delete permissions, invalid credentials and source grant revocation |
| `xml_copy_checks_source_customer_key_and_sets_destination_key` | Missing/wrong source keys preserve the destination; successful copy uses a distinct destination key and preserves the source |
| `xml_copy_obeys_object_size_limit_and_preserves_existing_destination` | Copy above the configured size cap preserves existing destination metadata and bytes |
| `xml_copy_v4_signature_binds_source_and_destination_conditions` | Valid signed copy, source/condition tampering rejection and create-only replay failure |

Shared `server::copy::tests` separately cover pinning during source replacement,
publication-time grant/condition changes, cancellation, deadline admission and
shutdown recovery. `storage::catalog::derived::tests` cover source deletion with
restart, staged transfer recovery and atomic publication failures. These checks
are shared-engine evidence, distinct from the XML wire tests above.

Executed on 2026-10-30:

| Command | Result |
| --- | --- |
| `cargo check --lib --tests` | PASS |
| `cargo test --test xml_storage_http` | PASS, 13 combined LIST/COPY tests |
| `cargo test --lib auth::signature::tests::v2` | PASS, 3 tests |
| `cargo test --lib server::copy::tests` | PASS, 9 tests |
| `cargo test --lib storage::catalog::derived::tests` | PASS, 22 tests |
| `cargo test --test auth_http xml` | PASS, 46 tests |
| `cargo xtask lint` | PASS, zero production violations |
| `cargo fmt --all -- --check` | PASS |

HTTP tests ran with local listener access enabled. The adapter adds no new
durable-write steps, so no new power-loss boundary or performance claim is made.
