# Cross-protocol object state

<!-- markdownlint-configure-file {"MD013": false} -->

Scope: the object portion of AUDIT-008. Bucket state is covered by the [Storage v2 bucket contract](grpc-v2-buckets.md). This slice adds an integration gate only; it adds no route, setting or dependency.

## Acceptance contract

Each object is written through one transport and observed or mutated through the others against one running emulator. JSON and XML requests are raw HTTP so wire representations are asserted directly. Storage v2 metadata uses `GetObject` on the wire through the SDK's registered pinned descriptors; v2 bytes, writes, updates, listing and deletion use the ordinary Go `storage.NewGRPCClient`. Legacy v1 uses the pinned genproto client and observes metadata and listings only, because its supported subset has no media RPC.

- Write: a JSON multipart upload, an XML `PUT /BUCKET/OBJECT` and a Storage v2 Writer each publish an object with content type and custom metadata. JSON, XML, Storage v2 and v1 then observe identical generation, metageneration, size, CRC32C, MD5, content type and custom metadata, and JSON, XML and Storage v2 return identical bytes, for both the live object and an explicitly selected generation. JSON int64 fields must be decimal strings; XML uses `x-goog-generation`, `x-goog-metageneration`, `x-goog-stored-content-length` and `x-goog-hash`; gRPC uses int64/fixed32/bytes fields.
- List: JSON, Storage v2 and v1 listings return the same names and generations after writes, mutations and each deletion. XML bucket listing is outside the supported subset and returns 400 `InvalidArgument` rather than an empty listing.
- Metadata mutation: a JSON PATCH conditioned on metageneration 1 bumps the metageneration to 2 and merges metadata for XML, Storage v2 and v1; a Storage v2 `UpdateObject` conditioned on the pre-PATCH metageneration fails FAILED_PRECONDITION. A Storage v2 `UpdateObject` likewise bumps the metageneration observed by JSON, XML and v1, and a JSON PATCH conditioned on the stale metageneration fails 412. Rejected mutations leave every view unchanged.
- Generation preconditions: a generation observed through XML satisfies `ifGenerationMatch` on JSON, Storage v2 and v1 reads; a non-matching generation fails with JSON 412, XML 412 `PreconditionFailed`, and FAILED_PRECONDITION on both gRPC services. An XML overwrite conditioned on the Storage v2-observed generation creates a new generation (metageneration 1, new bytes and hashes) on every transport; stale overwrites through XML, JSON and Storage v2 then fail with 412, 412 and FAILED_PRECONDITION and change nothing.
- Delete: Storage v2 deletes the JSON-written object, JSON deletes the XML-written object and XML deletes the Storage v2-written object, each conditioned on a generation that another transport observed. A stale generation fails first with the deleting transport's error (FAILED_PRECONDITION or 412) and leaves the object visible. After deletion the live name and the selected generation return JSON 404, XML 404 `NoSuchKey`, and NOT_FOUND on both gRPC services.
- Versioning: in a versioned bucket, an XML write is overwritten by a Storage v2 write conditioned on the XML generation, and the live object is deleted through JSON. The live name is NOT_FOUND/404 everywhere while both noncurrent generations remain readable with their own bytes and metadata through JSON, XML, Storage v2 and v1, and appear in `versions=true` listings on JSON, Storage v2 and v1 but not in live listings. Deleting one noncurrent generation through XML `DELETE ?generation=` removes only that generation everywhere.
- Strict authorization: the admin writes one object through each transport. `local-viewer` (objectViewer) gets the same metadata, bytes and listings through JSON, XML, Storage v2 and v1. `local-creator` (objectCreator, no `storage.objects.get`) is denied metadata, media and listing with JSON 403, XML 403 `AccessDenied` and PERMISSION_DENIED on both gRPC services. Anonymous callers receive JSON 401, XML 401 `AuthenticationRequired` and UNAUTHENTICATED on both gRPC services. Viewer and anonymous overwrites are denied on every transport, and the admin-written generation stays unchanged everywhere afterwards.

## Anonymous denials

The strict test first exposed that Storage v2 object RPCs reported anonymous denials as PERMISSION_DENIED, while JSON, XML, legacy v1, Storage v2 bucket RPCs and `BidiReadObject` reported an unauthenticated caller. The v2 object I/O and update/copy adapters passed `authorize_bucket` failures to the status converter without the shared `anonymous_error` mapping. Both adapters now apply it, so an anonymous caller without access receives UNAUTHENTICATED on every v2 object RPC, including CancelResumableWrite, and an authenticated caller without the permission still receives PERMISSION_DENIED.

## Scope limits

XML bucket listing and legacy v1 media are outside their supported subsets, so those projections are excluded. Derived-object operations (rewrite, compose and move) and legacy v1 writes are verified per transport by their own rows, not by this cross-transport matrix.

## Verification

On 2026-10-28:

- `cargo check --locked --offline`: PASS.
- `GCS_MIMIC_GO_TEST_RUN='^TestCrossProtocol' bash scripts/sdk-go.sh`: PASS, `TestCrossProtocolObjects`, `TestCrossProtocolObjectVersions` and `TestCrossProtocolObjectsStrict`.
- `GCS_MIMIC_GO_TEST_RUN='^TestGRPC' bash scripts/sdk-go.sh`: PASS, including the anonymous UNAUTHENTICATED checks for UpdateObject and CancelResumableWrite.
- `RUST_TEST_THREADS=8 just check` and `just check-sdk`: PASS on the final sources, 538 library tests; the only Go skip is the existing HTTPS-only V2 signed-transfer case.
- `cargo test --locked --offline --test auth_http xml`: PASS, 43 tests (XML/JSON interplay for PUT, reads, ranges, preconditions, deletes and strict permissions).
- `cargo test --locked --offline --test object_versioning_http`: PASS, 9 tests.
- `cargo clippy --locked --offline --all-targets -- -D warnings`: PASS.
- `go vet ./...` and `gofmt -l .` in `sdk-tests/go`: PASS, no findings.

No HTTPS profile, container build or real-cloud run was performed.
