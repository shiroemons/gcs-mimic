# Basic JSON contract final integration evidence

<!-- markdownlint-configure-file {"MD013": false} -->

Verified on **2026-10-23** against the [frozen basic local profile](../slices/basic-json-contract.md).
The integrated test source is `b309f9c2a50483885b362a053eefa74129de8f52`.
Application code, dependency manifests/lockfiles, SDK tests and comparison tooling
are unchanged from the merged inputs. Subsequent changes are compatibility ledger,
generated views, documentation and captured evidence only. The
[feature ledger](../../../compatibility/features.json) owns implementation status.

## Delivery completion conditions

| Condition | Completed scope | Evidence |
| --- | --- | --- |
| 1 | One native live-resource profile freezes bucket CRUD/labels/default class; media/multipart/JSON-resumable upload; object metadata GET/PATCH/PUT, whole/single-range reads, prefix/delimiter/offset paging and delete; strict local project-plus-bucket grants are explicit | [Profile, method tables and gap map](../slices/basic-json-contract.md); full [Rust](basic-json-contract-checks.txt), [Go](basic-json-contract-sdk-go.txt), [container](basic-json-contract-containers.txt) and [guide](basic-json-contract-guide.txt) runs |
| 2 | Supported response fields and read ETag validators remain delivered, including policy GET, selected generations, ranges and combined query conditions | [Original final fields/read-validator evidence](basic-json-fields-etag.md); unchanged source and SDK cases pass again in this final Rust/Go set |
| 3 | Supported conditions, atomic mutations, metadata merge/replacement, calculated/supplied hashes, query-bound paging, strict grants and complete failed-overwrite preservation pass; local ambiguous statuses/timing remain separately labeled | Acceptance tables below; [Rust](basic-json-contract-checks.txt), [Go](basic-json-contract-sdk-go.txt), [guide](basic-json-contract-guide.txt) |
| 4 | Every remaining field/header/error family has a method PENDING case or named gap; all original acceptance results, tests, verification and deviations are preserved | [Complete gap and migration tables](../slices/basic-json-contract.md); [ledger](../../../compatibility/features.json); preservation audit and validate/render/check in [checks](basic-json-contract-checks.txt) |

Order 8 is closed. The next slice is order 9 **Object copy**; copy, rewrite and
compose are not implemented by this integration. Closure of this bounded gate
does not complete the later basic API/CORS/HTTPS/Go-and-Ruby milestone.

## Inputs and final results

- Host: macOS ARM64, `aarch64-apple-darwin`; Rust/Cargo **1.99.1**, edition 2024.
  Exact compiler commit, LLVM and tool configuration are captured in the checks log.
- Go **1.27.3**, Storage **v1.70.0**, generated API **v0.308.0**, OAuth2 **v0.37.0**.
  The SDK runner guards isolated local endpoints and disables cloud credential
  and metadata-server discovery. No real Google bucket was contacted.
- Docker client/server **29.8.1**, Compose **v5.5.1**, buildx **v0.37.1**.
  Linux ARM64 execution is native; Linux AMD64 uses emulation.
- Final dependency freshness: **31 CURRENT**, zero exceptions or pin changes.
  The start attempt stopped at an upstream GitHub HTTP 403; the final complete
  automated check passes after the rate limit reset. Both actual outputs remain
  in the single dependency log.

| Command / journey | Executed result | Captured output |
| --- | --- | --- |
| `cargo test --workspace` after both non-fast-forward merges | PASS, exit 0; 459 tests, zero failed/ignored | Subsequent final workspace execution includes all merged cases below |
| `cargo test --workspace`, `cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings` after test completion | PASS, exit 0; 462 tests, zero failed/ignored; no lint warnings | Superseded by the complete final check output below |
| `just check` | PASS, exit 0; 462 tests, zero failed/ignored; fmt, workspace clippy, ledger/generated-view, shell syntax and mocked release checks | [checks](basic-json-contract-checks.txt) |
| `just check-sdk` | PASS, exit 0; Go vet, 35 top-level tests and 73 subtests; zero failures/skips | [Go](basic-json-contract-sdk-go.txt) |
| `just check-containers` | PASS, exit 0; both architecture builds, read-only-root ordinary and seeded Compose, exact media bytes and stop/start/recreate persistence | [containers](basic-json-contract-containers.txt) |
| `cargo build --release --locked --offline --bin gcs-mimic` and unchanged guide blocks extracted by heading | PASS, exit 0; native and final ARM64 image, bucket/object conditions and metadata, multipart/resumable, paging, error/fields checks and restart equality | [guide](basic-json-contract-guide.txt) |
| `GCS_MIMIC_REFERENCE_REPO=https://github.com/fsouza/fake-gcs-server bash scripts/compare-reference-emulator.sh` | PASS, exit 0; 4 targets × 302 requests = 1,208 raw records, with existing PATCH/PUT/fields/read-validator assertions | [comparison](basic-json-contract-comparison.txt) |
| `env -u RUSTUP_TOOLCHAIN cargo xtask dependencies check` | Final PASS, exit 0; all 31 compiler/library/SDK/Action/container inputs CURRENT | [dependencies](basic-json-contract-dependencies.txt) |
| `cargo xtask compatibility validate`, `render`, `check` | PASS, exit 0; 591 features, 401 parameter locations and 357 schema fields; both generated views current | [checks](basic-json-contract-checks.txt) |
| Markdown lint, diff whitespace, original-ledger preservation and local-path scans | PASS; prose and generated tables lint clean; original cases/deviations retained; no local-path matches | [checks](basic-json-contract-checks.txt) |

All final heavy checks ran once on the final application/test/SDK inputs. Only
affected guide execution was retried after correcting its harness readiness
wait and applying the guide's documented fixed-port recreation step. No runtime
or SDK source changed, and no negative test was weakened, removed or skipped.
No image/package was published. Logs replace local checkout, home and temporary
path prefixes with placeholders and declare that once at the top; other output
is preserved. Raw Docker progress whitespace is retained. Markdown lint retains
the existing prose exemptions; generated views use MD013 disabled for long
generated content and MD024 restricted to siblings because categories repeat
under different feature kinds.

| Final local image | Actual image index digest |
| --- | --- |
| Linux AMD64 smoke | `sha256:80bc6dd4bd7a5f8f1bf24eda24fd6a42783b429eae8f895ed57bfc56785d9a1f` |
| Linux ARM64 smoke and guide | `sha256:923abfbfded7797d305ceca4a499ee66efa62974811c7347159d512529d3dbcb` |

Docker inputs remain pinned: Rust
`aab42391a986424e22d6a3ec14f37bc6dd3dacff4d457ca7cccd781e02297cc6`,
distroless `83b9b50586253821cf63ffcec87d3d94265a093ad30da232039af962dd8f4edf`,
frontend `4edf897a3ffa55b89f906fc8cc78afdb3f1834cc9c7083565e611a8a7d5fe99e`
and test curl `11e64e28c5f58c8d93ee95fcf22ac055f48b0bc440e0ce7d8013bbb5ad6aa8ac`.
The dependency log verifies index/platform freshness; the checks log captures
manifest, lockfile, toolchain, Dockerfile and test-file SHA-256 values.

## Documented acceptance scopes

The following tests pass in the final Rust or Go run. The ledger case descriptions
repeat the contract's assertion filters: a test containing a local parser/status/
timing assertion contributes only its documented assertions to an EXACT row.

| Acceptance case | Contributing executed tests / assertions |
| --- | --- |
| BASIC-CONDITION-READ | `obj_http_all_preconditions_read_304_mutation_412_and_missing_precedence`, `bkt_http_crud_metadata_conditions_and_errors`: single GET condition pass/412/bodyless 304 only; `basic_contract_generation_match_zero_rejects_live_reads_and_metadata_mutations`: isolated zero-only metadata/media GET 412 and unchanged full resource/bytes |
| BASIC-CONDITION-MUTATION | `basic_contract_object_generation_match_matrix_preserves_failed_resources`, `basic_contract_object_metageneration_match_matrix_preserves_failed_resources`, `basic_contract_bucket_metageneration_match_matrix_preserves_failed_resources`: each method independently passes/fails, preserving complete failed resources; the zero-only test also rejects PATCH/PUT/DELETE. Local NotMatch status, DELETE 204 and metadata omission defaults do not contribute |
| BASIC-CONDITION-ATOMIC | `obj_http_concurrent_create_if_absent_has_one_winner`, `obj_multipart_http_concurrent_create_if_absent_has_one_winner_and_no_leaks`, `bucket_patch_conditioned_race_has_one_winner`, `bucket_put_conditioned_race_has_one_winner`: single Match one-winner comparisons and preservation, without provider streaming/check-timing claims |
| BASIC-CONDITION-SDK | `TestObjectMediaLifecycle`, `TestObjectMultipartWriter`, `TestObjectMetadataUpdate`: successful comparisons/create-if-absent and stale Match rejection |
| BASIC-HASH-VECTORS | `empty_and_multi_chunk_bodies_have_exact_bytes_and_incremental_hashes`, `obj_http_media_and_multipart_independent_checksum_vectors_across_chunks`: independent empty/binary/streaming vectors, full-byte hashes and bytes |
| BASIC-HASH-MEDIA | `basic_contract_media_hash_matrix_validates_each_checksum_and_preserves_failed_overwrites`: explicit Content-Type; isolated MD5/CRC32C/both success, wrong valid checksums 400, absent new object and full failed-overwrite preservation |
| BASIC-HASH-MULTIPART | `basic_contract_multipart_metadata_only_checksums_validate_stored_bytes`: isolated metadata MD5-only/CRC32C-only, no duplicates/header/final part. `obj_multipart_http_checks_metadata_header_and_final_part_hashes_before_publication`: only valid metadata mismatch and CRC32C-only third-part success/mismatch/preservation |
| BASIC-HASH-RESUMABLE | `basic_contract_single_request_resumable_checksums_validate_stored_bytes`: one data request, isolated MD5/CRC32C final headers, exact wire hashes/size/bytes. `obj_http_resumable_independent_empty_and_multichunk_checksum_vectors`: wire CRC32C/bytes for empty and two data requests, excluding multi-request MD5. `checksum_and_final_metadata_failures_preserve_the_previous_full_resource`: two valid single-hash mismatch 400/preservation cases only |
| BASIC-HASH-NO-PARTIAL-PUBLISH | `producer_closure_without_eof_never_publishes_partial_bytes`, `obj_http_disconnected_overwrite_preserves_the_previous_generation`, `obj_multipart_http_truncated_checksum_part_preserves_existing_generation`: EOF/framing failure never replaces old data |
| BASIC-HASH-SDK | `TestObjectMediaLifecycle`, `TestObjectMultipartWriter`, `TestObjectResumableWriter`, `TestObjectResumableChecksumFailurePreservesObject`: supported checksum/resource-byte assertions, excluding multi-request resumable MD5 and local error/finalization timing |
| BASIC-ERROR-SHAPE | `basic_contract_json_errors_keep_complete_envelopes_with_fields_selection`, `basic_contract_strict_denials_keep_complete_json_envelopes_with_fields_selection`, `fields_http::fields_invalid_selectors_and_errors_never_filter_or_mutate`: complete numeric-code/message/errors structure and unfiltered errors. The encoded missing-name request has a generic message; `json::tests::bkt_err_001_envelope_content_type_and_catalog_mappings` supplements quote/newline message escaping. Exact Content-Type/auth/parser/hash reasons excluded |
| BASIC-ERROR-PAIRS | Matrices and existing bucket/object tests above: unconditional authorized 404 notFound and isolated Match 412 conditionNotMet. `obj_range_http_unsatisfiable_empty_and_ignored_ranges`: valid bytes=10-/bytes=100-200 against ten bytes, 416 requestedRangeNotSatisfiable only |
| BASIC-ERROR-EMPTY | Single Match object/bucket matrices: successful DELETE body emptiness only. Existing single read NotMatch GET checks: bodyless 304. Fields DELETE test: unconditional emptiness only; exclude status 204 selection, mixed validators and XML |

The mutation success matrices additionally distinguish PATCH merge from PUT
replacement, verify replacement size/MD5/CRC32C, check dropped/preserved metadata
and fixed content fields, and preserve bucket identity fields. These support the
local profile without settling undocumented omission/default rules.

## Local interaction and failure preservation

| Local verification family | Final executed tests |
| --- | --- |
| Malformed/duplicate conditions, every applicable method | `obj_http_malformed_conditions_reject_every_object_method_without_mutation`, `bkt_http_malformed_conditions_preserve_all_bucket_methods` |
| Changed conditions during streamed media/multipart | `obj_http_media_and_multipart_recheck_each_condition_after_streaming` |
| Competing bucket delete/PATCH/PUT and object delete/PATCH/PUT | `bkt_http_conditional_delete_and_metadata_mutation_have_one_winner`, `conditional_delete_and_metadata_patch_have_one_winner`, `all_conditions_generation_selectors_and_conditioned_races`; the object DELETE/PATCH test alternates main/worker placement, without claiming a forced interleaving or both winners in every run |
| Failed media/multipart/resumable overwrites | `obj_http_failed_media_and_multipart_overwrites_preserve_full_resource_without_leaks`, `obj_http_resumable_failed_overwrites_preserve_metadata_bytes_and_files`; complete metadata, bytes and blob/staging state survive hash/size/body failure |
| Strict grant revocation and method coverage | `iam::auth_http_media_and_multipart_recheck_revoked_overwrite_grants_after_streaming`, `iam::auth_http_revoked_bucket_grants_deny_every_basic_method_and_preserve_objects`; bucket/object snapshots and zero rejected session rows, inspected through a read-only catalog connection |
| Stored resumable conditions and one finalization winner | `all_four_initiation_conditions_are_rechecked_atomically_and_cannot_be_relaxed`, `two_create_if_absent_sessions_have_one_finalization_winner`; finalization timing and extra-query rejection remain local evidence, not promoted condition fidelity |
| Query-bound paging and metadata semantics | `object_list_tokens_bind_query_and_reject_corruption`, `versioned_listing_pages_and_query_boundaries`, `bkt_http_sorted_scoped_pagination_and_limits`, `patch_merges_put_replaces_and_download_reflects_persistent_metadata`, `metadata_boundaries_encoding_negotiation_and_reupload_defaults` |

The guide executes unchanged first create/PATCH/PUT/versioning-hold blocks,
object upload/condition/read errors, fields/ETags, multipart, both host-resumable
blocks, cancellation and list/conditional metadata blocks. Native restart
compares full metadata and bytes; Docker additionally runs the unchanged
stop/start/down/up/deletion block. The final ARM64 smoke image is reused without
rebuilding. Existing examples already cover conditions, checksums and errors,
so no guide content was added. Broader peer/browser/strict walkthroughs were not
rerun as guide blocks; their applicable wire and pinned SDK tests passed.

## Reference comparison and source review

The existing executed corpus uses fake-gcs-server **v1.56.1**, commit
`896ece51e49eed0946089f3089f6343855c0d106`, archive SHA-256
`13fa1ed9cff0216baedaedb162b3cf37575bba955fb55fd4b76acb9b818494dc`,
with filesystem/memory backends and both local profiles. It retains all 1,208
raw responses. It does not execute the newly added checksum/mutation/error
comparison rows; those other-product cells explicitly say **source review only**.
gcp-storage-emulator **v2026.07.19** was inspected, not executed.

Source review finds upload generation conditions but no matching bucket/object
metadata/delete coverage in the pinned fake handlers; its multipart/resumable
metadata hashes are checked before publication, while media/final X-Goog-Hash
validation is absent. Its shared reason mapper lacks requestedRangeNotSatisfiable.
The pinned gcp handlers do not evaluate condition queries; metadata hash mismatches
raise Conflict before create_file, and some failures return only error.message.
These partial scopes do not receive unsupported checkmarks.

| Fetched pinned source | SHA-256 |
| --- | --- |
| [fake upload](https://github.com/fsouza/fake-gcs-server/blob/v1.56.1/fakestorage/upload.go) | `8713b9787c5cf48b22700f332efc847658e3200d6d74e3fd081b7761783e424c` |
| [fake object](https://github.com/fsouza/fake-gcs-server/blob/v1.56.1/fakestorage/object.go) | `cfd884eb03aa429f6b681bef8ca1923c600b24beb850c0620121c0c23110a56b` |
| [fake bucket](https://github.com/fsouza/fake-gcs-server/blob/v1.56.1/fakestorage/bucket.go) | `73ac2fded764470f13b451d281c61089704f48e79a0234cfdf164657d443cf96` |
| [fake response](https://github.com/fsouza/fake-gcs-server/blob/v1.56.1/fakestorage/json_response.go) | `e99ac0ac585905ad6f8fd7392fe19224aa83877f5d7d4acaf4052f5fb09435d9` |
| [gcp object](https://github.com/oittaa/gcp-storage-emulator/blob/v2026.07.19/src/gcp_storage_emulator/handlers/objects.py) | `e382f1715e55805a0b7459c1f9c8f21b8f38443da6c8d863ea6994bf1c1f4c4f` |
| [gcp bucket](https://github.com/oittaa/gcp-storage-emulator/blob/v2026.07.19/src/gcp_storage_emulator/handlers/buckets.py) | `ce160c2cffdc84937d868a80ea91bed8141b5c2eae9cd623db228cf9fcfec01f` |
| [gcp response/router](https://github.com/oittaa/gcp-storage-emulator/blob/v2026.07.19/src/gcp_storage_emulator/server.py) | `b0227517548e0c34304b06029712351de277bd2a8bb1c7c45e71d0a27ec55837` |

Official authority and Discovery/protobuf byte pins remain in the frozen contract:
Discovery revision `20261013`, SHA-256
`1f74b7a93b3d2872cc332fc20dafae1657fc255930ce336618bea3d0b4d95ae0`;
Storage v2 proto file-changing commit
`d73dfb3c3be52245625b951b9c0bcf65163c549f`, SHA-256
`a0df6132967b97a89be248f1f24d277ee8df8f8d885db1d4af6249469ff0c1cf`.
Emulator source review is comparison evidence, not Google contract authority.

## Ledger outcome and limitations

| Rows | Final status / concrete remaining boundary |
| --- | --- |
| BEHAVIOR-COMMON-011 | IMPLEMENTED / EXACT: 4 documented condition cases, complete linked Rust/Go evidence and no in-scope deviations |
| BEHAVIOR-OBJECT-005 | IMPLEMENTED / EXACT: 6 documented hash cases, complete linked Rust/Go evidence and no in-scope deviations |
| BEHAVIOR-COMMON-007 | IMPLEMENTED / EXACT: 3 envelope/status-pair/body cases, linked wire evidence and no in-scope deviations |
| BEHAVIOR-COMMON-004/012, BEHAVIOR-OBJECT-010..014/016 | Remain IMPLEMENTED in their existing scopes; no scope expansion |
| JSON-BUCKETS-001/002/005/006/008/013; JSON-OBJECTS-004/005/007/008/010/015 | Remain PARTIAL: advanced fields/projection/ACL, strict provider-wire details, local defaults/input edges and the named condition/checksum/error gaps remain explicit |
| BEHAVIOR-COMMON-001/002/003/005/006/009/013/014; BEHAVIOR-OBJECT-001/002/003/006/007/008/009/015/018 | Remain PARTIAL for the concrete naming/project/parser/metadata/history/paging/header/retry blockers enumerated in the frozen contract; no other behavior promoted |
| DESIGN-070/073/074/082 | Remain PARTIAL: full resource/platform/power-loss/SDK breadth is unverified; migrated timeout/reclaim/transport limits remain visible |

The preservation audit compares the pre-integration ledger with the final ledger:
all **835 original acceptance cases** retain description content, results and
test IDs somewhere at their explicit destination; all **235 original deviations**
retain their text, with stale-capability clarification appended. Original
verification records (**372**) and SDK records (**92**) are retained. The three promoted rows have unchanged
EXACT targets, COMPLETE acceptance, only PASS cases, no deviations, no unresolved
dependencies and `last_verified: 2026-10-23`.

Open limits include SPEC-008 mutation NotMatch 304/412 conflict and mixed/missing
precedence, SPEC-009 ordinary-prefix redirects, SPEC-011 missing request MIME,
SPEC-012 opaque IDs/headers and local DELETE status, SPEC-010/016 checksum parser
and multipart header rules, and SPEC-RSM-EDGES multi-data-request MD5.
The objects.insert subsequent-PUT sentence introduces query parameters but calls
them “these headers”; its condition-query applicability remains unresolved.
Extra session-query 400 is wire-tested only for PUT ifGenerationMatch; stored
condition/permission finalization timing remains local. Real-cloud differential,
Ruby/Rails, performance/cgroup and power-loss tests were not run.
