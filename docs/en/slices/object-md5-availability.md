# Object MD5 availability

<!-- markdownlint-configure-file {"MD013": false} -->

## Contract and scope

Checked on 2026-10-30 against the official [object metadata](https://docs.cloud.google.com/storage/docs/metadata), [multipart completion](https://docs.cloud.google.com/storage/docs/xml-api/post-object-complete), [data validation](https://docs.cloud.google.com/storage/docs/data-validation) and [June 23, 2023 release note](https://docs.cloud.google.com/storage/docs/release-notes#June_23_2023) contracts. Multipart completion provides CRC32C, no MD5 checksum, and an ETag that is not derived from MD5. Multipart creation does not imply composite component metadata. Copy and rewrite of multipart objects are supported by Google; source data receives server-side integrity validation.

The fetched HTML SHA-256 digests are `c2b1e4cfa1dd508c9db50f0fcb838b394e4a695586a89fa2137648f8c3af230e` for multipart completion and `6df27df3956536005d8c301f044b833c21b9987104a22233ffaa627da5251ab9` for object metadata. The reference emulator remains fake-gcs-server v1.56.1 at `896ece51e49eed0946089f3089f6343855c0d106`; it does not define the Google checksum contract. The existing Discovery/protobuf pins remain unchanged. No dependency is added.

This shared slice supports BEHAVIOR-OBJECT-005, BEHAVIOR-OBJECT-006 and EXT-XML-020. JSON object resources, JSON/XML media headers, XML ETags and both gRPC object representations share the same selected-record checksum decision. Full XML multipart protocol behavior belongs to its adapter and session slice. Feature status remains owned by `compatibility/features.json`.

## Representation and persistence

`ObjectRecord.md5` always retains the internal computed digest. Incremental MD5 and CRC32C computation, declared checksum verification, immutable blobs, authorization and publication conditions remain in the shared storage path. The new two-variant `Md5Availability` marker suppresses public MD5 independently of `component_count`. Its default preserves old serialized records and response replay snapshots. `has_md5()` combines the explicit marker with the existing composite exclusion, so ordinary and composite wire representations retain their previous behavior.

Schema 36 appends `md5_available`, defaulting to 1 for legacy eligibility. A non-null component count still excludes public MD5 independently of that legacy default. Values other than 0 and 1 are rejected, and explicit suppression cannot coexist with a component count. Schema migration is transactional and precedes XML multipart schema 37. Schema-inventory validation and future-version rejection retain their existing read-only preflight. Test downgrade helpers remove the newer multipart tables and marker before reconstructing older schema fixtures.

Multipart completion sets `Unavailable` in the existing prepared publication callback, before the object row is inserted. It adds no file publication, rename or sync step. Restart, metadata reads, lists and metadata updates retain the marker. JSON omits `md5Hash` without inventing `componentCount`; gRPC retains CRC32C and leaves MD5 empty. JSON and XML hash headers contain CRC32C only. Matching customer-supplied encryption keys cannot make an unavailable MD5 visible. Multipart ETags use the local opaque `m-{generation}-{metageneration}` form; Google does not promise that spelling. Existing ordinary and composite ETag formats remain unchanged.

JSON read validators use the returned opaque ETag. XML read validators remain outside the existing supported XML subset and continue to fail explicitly; this metadata slice does not add XML conditional-read support.

## Derived objects and remaining evidence

The current local copy/rewrite path verifies its internal source digest and CRC32C, recomputes destination digests while streaming, and publishes an ordinary destination when the source has no composite component count. That destination exposes its newly computed MD5. Composition creates the existing composite identity and continues to hide MD5. The source multipart record keeps its explicit absence marker.

Google's public pages establish that multipart objects may be copied/rewritten and describe source checksum validation, but do not establish the destination MD5-availability detail used here. This is a declared local interpretation, pending explicitly authorized provider differential evidence. No full provider parity, SDK/container run, or measured performance improvement is claimed by this slice. SPEC-012 and the owning multipart gaps must retain that limitation.

## Acceptance

- Ordinary objects keep MD5, CRC32C and their existing ETag; composite objects retain their component counts and absent MD5.
- Explicit MD5 absence survives shared publication and restart while preserving the actual internal MD5, CRC32C and bytes.
- JSON and gRPC omit MD5 without composite metadata; JSON media headers contain CRC32C only, including matching-key metadata responses.
- Old serialized records without the marker remain readable, and the internal marker is absent from API resources.
- Migration preserves legacy object values, rolls back completely, and can then be retried; invalid SQL marker values and conflicting composite markers fail.
- Existing schema, durability, condition and authorization regressions remain applicable. No additional durable filesystem step was introduced.

Focused checks are `cargo check --tests`, `cargo test --lib md5_availability`, the JSON object-resource tests, and the gRPC conversion tests. Integrated XML completion/read/conditional/restart cases are owned by the multipart adapter tests. Final command results are reported with the integrated validation, without claiming execution of unrun SDK or container checks.

Validation on 2026-10-30: `cargo check --tests` passed, the two focused migration/restart tests passed, the two affected bounded-list fixture tests passed, and `cargo xtask lint` reported zero production violations. Formatting checks on the changed Rust files, `git diff --check`, and Markdown lint for this slice passed. The final `cargo test --lib` ran 740 tests: 739 passed, including the expanded restart/copy/rewrite test and the timestamp/retention regressions. The remaining failure was the independently added `auth::signature::tests::v2_multipart_signatures_bind_upload_and_part_subresources` assertion at `src/auth/signature/tests.rs`: actual `Err(Malformed)` versus expected `Err(Mismatch)`. That signature-parser test belongs to the multipart adapter work and was reported to its owner; the shared MD5 slice does not change signature parsing. SDK, container and real-provider differential checks were not run for this slice.
