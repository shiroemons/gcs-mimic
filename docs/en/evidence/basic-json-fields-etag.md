# JSON fields and read ETag final integration evidence

<!-- markdownlint-configure-file {"MD013": false} -->

Verified on **2026-10-23**. Application source is
`6d97e842725b4a479dbeaa6d8285706e1310903d`; the remaining stale unsupported-test
expectation is corrected in `e179de800ffcb2c230b086eb71dabe0f7ff4b028`.
The executed comparison harness is
`861a8582b2b11f1b3ecc02c269e4014b1ee25463`.
The final runs below use the same application, SDK tests, manifests and lockfiles;
subsequent changes are ledger, generated views, documentation and evidence only.

The [fields contract](../slices/json-response-fields.md) and
[read validator contract](../slices/json-read-etag-validators.md) define the local
subset and the remaining provider questions. The
[feature ledger](../../../compatibility/features.json) owns statuses.
MD013 is disabled to preserve complete test IDs, digests and evidence tables.

## Inputs and final results

- Native host: macOS ARM64, `aarch64-apple-darwin`.
- Rust/Cargo **1.99.1**, edition 2024; rustc commit
  `58cf8f9c35b355199727627d07ae34a4e2846d67`, LLVM 23.1.1.
- Go **1.27.3**, Storage **v1.70.0**, generated API **v0.308.0**,
  OAuth2 **v0.37.0**. SDK requests use isolated guarded local endpoints;
  inherited cloud credential/metadata-host variables were cleared.
- Docker client/server **29.8.1**, Compose **v5.5.1**, buildx **v0.37.1**.
  ARM64 execution is native; AMD64 uses emulation.
- No dependency or lockfile changed. Start and final freshness checks report
  all **31** monitored inputs CURRENT, without exceptions.

| Command or journey | Executed result | Raw evidence |
| --- | --- | --- |
| `bash scripts/test-check.sh` then `RUSTUP_TOOLCHAIN=1.99.1 bash scripts/check.sh` | PASS, exit 0: 439 Rust tests, zero failed/ignored; fmt, clippy, compatibility, shell and mocked release checks | [checks](basic-json-fields-etag-checks.txt) |
| `GOPROXY=off GOFLAGS=-mod=readonly bash scripts/sdk-go.sh` with inherited cloud variables cleared | PASS, exit 0: Go vet, 35 top-level tests and 73 subtests; zero failures/skips | [SDK](basic-json-fields-etag-sdk-go.txt) |
| `bash scripts/smoke-containers.sh` | PASS, exit 0: AMD64/emulated and ARM64/native builds, ordinary and seeded Compose, persistent restart/recreate | [containers](basic-json-fields-etag-containers.txt) |
| Unchanged first create, object and fields/validator guide blocks against the native release binary | PASS: exact upload/MD5, selected metadata/list, bodyless 304, stale 412, then identical bytes and validator after process restart | [guide](basic-json-fields-etag-guide.txt) |
| Same unchanged blocks against the final ARM64 Compose image | PASS: same assertions, then stop/start retains matching 304, stale 412 and exact bytes; no image rebuild | [guide](basic-json-fields-etag-guide.txt) |
| `GCS_MIMIC_REFERENCE_REPO=https://github.com/fsouza/fake-gcs-server bash scripts/compare-reference-emulator.sh` | PASS, exit 0: 4 targets × 302 requests = 1,208 raw records; all fields/ETag and existing PATCH/PUT assertion groups pass | [comparison](basic-json-fields-etag-comparison.txt) |
| `env -u RUSTUP_TOOLCHAIN cargo xtask dependencies check` at start and after final verification | PASS, exit 0: all 31 inputs CURRENT, no pin changes | [dependencies](basic-json-fields-etag-dependencies.txt) |
| `cargo +1.99.1 xtask compatibility validate`, `render`, `check` | PASS: 591 rows, 401 parameter locations and 357 schema fields; English/Japanese views regenerated | [checks](basic-json-fields-etag-checks.txt) |
| Changed Markdown lint, whitespace and local-path scans | PASS: seven prose files and both generated matrices; zero lint issues and no path matches | [checks](basic-json-fields-etag-checks.txt) |

The repository has no Just recipes; these are its existing equivalent scripts.
The successful final workspace run includes the corrected negative test; no
test was removed, skipped or weakened. Final raw logs form one evidence set.
Each log declares replacement of checkout, home and temporary path prefixes on
its first line; other output is preserved. No image, package or cloud resource
was published or created.

Prose retains its existing MD013 exemptions for long tables. Generated matrices
disable MD013 and limit MD024 duplicate-heading checks to siblings because
categories repeat under different feature kinds. Docker progress output retains
upstream trailing spaces; raw-log whitespace checks disable only blank-at-eol,
while prose, JSON and generated views use the default whitespace checks.

Actual local image indexes:

| Platform | Image digest |
| --- | --- |
| Linux AMD64 smoke | `sha256:c2365b71e65fea795387e94f1d404b3558d5c78ae4830b8a77e34af1ca4e9837` |
| Linux ARM64 smoke and guide | `sha256:cc0a2de75987d3008e4b17c42bc8f22a0000f7d007cfdb67e05a7cd72eb122b1` |

Actual Docker input digests are verified in the dependency log: Rust
`aab42391a986424e22d6a3ec14f37bc6dd3dacff4d457ca7cccd781e02297cc6`,
distroless
`83b9b50586253821cf63ffcec87d3d94265a093ad30da232039af962dd8f4edf`,
Dockerfile frontend
`4edf897a3ffa55b89f906fc8cc78afdb3f1834cc9c7083565e611a8a7d5fe99e`
and test-only curl
`11e64e28c5f58c8d93ee95fcf22ac055f48b0bc440e0ce7d8013bbb5ad6aa8ac`.
The checks log records exact image IDs and unchanged application/SDK/schema
SHA-256 values.

## Acceptance mapping

Every PASS below is covered by the final workspace or full Go run. Test IDs use
the canonical slice tables; grouped rows name every included acceptance case.
Boundary tests establish local behavior, not undocumented Google wire parity.

| Fields acceptance case | Executed tests | Result and evidence |
| --- | --- | --- |
| FIELDS-001, FIELDS-002, FIELDS-010 | `json::fields::tests::fields_select_paths_subselectors_wildcards_and_omitted_values`; object HTTP cases | PASS, [checks](basic-json-fields-etag-checks.txt) |
| FIELDS-003, FIELDS-015 | `json::fields::tests::fields_reject_malformed_unknown_and_scalar_children`; `fields_invalid_selectors_and_errors_never_filter_or_mutate` | PASS, [checks](basic-json-fields-etag-checks.txt) |
| FIELDS-004 | `json::fields::tests::fields_bound_length_depth_and_selection_count` | PASS, [checks](basic-json-fields-etag-checks.txt) |
| FIELDS-005, FIELDS-006, FIELDS-007 | `fields_bucket_methods_and_policy_preserve_mutations` | PASS, [checks](basic-json-fields-etag-checks.txt) |
| FIELDS-008, FIELDS-009 | `fields_object_methods_lists_and_zero_byte_metadata` | PASS, [checks](basic-json-fields-etag-checks.txt) |
| FIELDS-011 | `fields_multipart_response_preserves_uploaded_metadata_and_bytes` | PASS, [checks](basic-json-fields-etag-checks.txt) |
| FIELDS-012 | `fields_resumable_final_response_survives_restart_and_replay` | PASS, [checks](basic-json-fields-etag-checks.txt) |
| FIELDS-013 | `fields_invalid_selectors_and_errors_never_filter_or_mutate` | PASS, [checks](basic-json-fields-etag-checks.txt) |
| FIELDS-014 | `fields_delete_is_ignored_media_is_explicitly_unsupported_and_xml_unchanged` | PASS, [checks](basic-json-fields-etag-checks.txt) |
| FIELDS-016 | `TestPartialResponseFields` | PASS, [SDK](basic-json-fields-etag-sdk-go.txt) |
| FIELDS-018 | `fields_read_etag_validators_use_the_complete_resource` | PASS, [checks](basic-json-fields-etag-checks.txt) |
| FIELDS-INPUT-ORDER | `fields_invalid_selection_precedes_read_etag_evaluation` | PASS, [checks](basic-json-fields-etag-checks.txt) |
| FIELDS-ROUTE-REJECTIONS, FIELDS-EMPTY-PARENT | `fields_invalid_selectors_leave_all_routes_unchanged` | PASS, [checks](basic-json-fields-etag-checks.txt) |
| FIELDS-PERSISTED-SYNTAX | `json::fields::tests::fields_persisted_selection_does_not_depend_on_current_schema`; resumable restart/replay HTTP case | PASS, [checks](basic-json-fields-etag-checks.txt) |
| FIELDS-RENDERER-SCHEMA | `json::fields::tests::fields_rendered_resource_paths_match_pinned_schemas` | PASS, [checks](basic-json-fields-etag-checks.txt) |
| FIELDS-017 | Provider edge fixture was not authorized or executed | PENDING on method rows and SPEC-FIELDS-EDGES |

| Read validator acceptance case | Executed tests | Result and evidence |
| --- | --- | --- |
| ETAG-READ-RESOURCES, ETAG-READ-MATCH-FAILURE, ETAG-READ-CACHE-RESPONSE, ETAG-READ-ORDER | `etag::json_etag_read_object_metadata_media_and_range`; `TestReadETagValidators` | PASS, [checks](basic-json-fields-etag-checks.txt), [SDK](basic-json-fields-etag-sdk-go.txt) |
| ETAG-READ-COMPARISON, ETAG-READ-SYNTAX | Both `http_conditions::tests::etag_comparison_*` cases; `etag::json_etag_read_syntax_and_missing_resources` | PASS, [checks](basic-json-fields-etag-checks.txt) |
| ETAG-READ-MISSING | `etag::json_etag_read_syntax_and_missing_resources` | PASS, [checks](basic-json-fields-etag-checks.txt) |
| ETAG-READ-BUCKETS | `etag::json_etag_read_buckets_and_metadata_changes_in_both_profiles`; `TestReadETagValidators` | PASS, [checks](basic-json-fields-etag-checks.txt), [SDK](basic-json-fields-etag-sdk-go.txt) |
| ETAG-READ-GENERATIONS, ETAG-READ-EMPTY-PUBLIC | `etag::json_etag_read_selected_generation_zero_byte_and_public_cache` | PASS, [checks](basic-json-fields-etag-checks.txt) |
| ETAG-READ-PERMISSIONS | `etag::json_etag_read_authorization_precedes_validators` | PASS, [checks](basic-json-fields-etag-checks.txt) |
| ETAG-READ-SNAPSHOT | `etag::json_etag_read_metadata_update_and_overwrite_pin_open_reader` | PASS, [checks](basic-json-fields-etag-checks.txt) |
| ETAG-READ-REJECTIONS | `etag::json_etag_read_unsupported_lists_dates_and_writes`; existing unsupported-request/header regressions | PASS, [checks](basic-json-fields-etag-checks.txt) |
| ETAG-READ-FIELDS, ETAG-READ-FIELDS-ORDER | Both combined fields/validator HTTP cases | PASS, [checks](basic-json-fields-etag-checks.txt) |
| ETAG-READ-DATE-PRECEDENCE | `etag::json_etag_read_date_precedence_and_unrelated_routes_preserve_behavior`; `etag::json_etag_read_if_match_overrides_if_unmodified_since` | PASS, [checks](basic-json-fields-etag-checks.txt) |
| ETAG-READ-QUERY-ORDER | `etag::json_etag_read_combined_query_conditions_use_one_selected_record`; `TestReadETagValidators` | PASS: Match failures 412 before NotMatch/If-None-Match 304, [checks](basic-json-fields-etag-checks.txt), [SDK](basic-json-fields-etag-sdk-go.txt) |
| ETAG-READ-IAM | `etag::json_etag_read_iam_policy_validators_preserve_fields_and_permissions`; `TestReadETagValidators` | PASS: local policy validator, permissions, invalidation, fields and date/If-Range rules, [checks](basic-json-fields-etag-checks.txt), [SDK](basic-json-fields-etag-sdk-go.txt) |
| ETAG-READ-COLLECTION-CONTRACT, ETAG-READ-PROVIDER-WIRE | No provider differential fixture executed | PENDING on method rows and named gaps |

The guide separately executes metadata `fields=name,size`, list
`fields=items(name),nextPageToken`, matching If-None-Match 304 and stale If-Match
412. Both native and Compose restarts preserve the cached validator and exact
media bytes. The broader onboarding, Ruby, real-service and performance journeys
were not rerun.

## Reference comparison and sources

Checked on **2026-10-23**. Official sources and their Discovery/protobuf byte pins
are in the two slice contracts. Discovery revision remains `20261013`.
Google's [partial response](https://docs.cloud.google.com/storage/docs/json_api#partial-response),
[JSON headers](https://docs.cloud.google.com/storage/docs/json_api/v1/parameters)
and [preconditions](https://docs.cloud.google.com/storage/docs/request-preconditions)
define the documented scope. RFC 9110 defines HTTP syntax/comparison and date
precedence. Exact provider wire questions remain explicit.

fake-gcs-server **v1.56.1**, commit
`896ece51e49eed0946089f3089f6343855c0d106`, archive SHA-256
`13fa1ed9cff0216baedaedb162b3cf37575bba955fb55fd4b76acb9b818494dc`,
was executed with both filesystem and memory backends. Both return complete
bucket/object/list resources despite fields selection, and 200 for the scoped
wildcard If-None-Match/stale If-Match requests. Both local profiles return the
selected fields, bodyless 304 and JSON 412. The raw comparison retains actual
request headers and bodies; normalized comparison is supplemental only.

gcp-storage-emulator **v2026.07.19** was reviewed from its pinned source only.
Its response/router serializes complete resources and its GET/download handlers
do not evaluate these validators. It was not executed: the existing comparison
harness supports fake-gcs-server's two backends, not that Python service.

Fetched source-file SHA-256 values:

| Pinned file | SHA-256 |
| --- | --- |
| [fake JSON response](https://github.com/fsouza/fake-gcs-server/blob/v1.56.1/fakestorage/json_response.go) | `e99ac0ac585905ad6f8fd7392fe19224aa83877f5d7d4acaf4052f5fb09435d9` |
| [fake object handlers](https://github.com/fsouza/fake-gcs-server/blob/v1.56.1/fakestorage/object.go) | `cfd884eb03aa429f6b681bef8ca1923c600b24beb850c0620121c0c23110a56b` |
| [fake bucket handlers](https://github.com/fsouza/fake-gcs-server/blob/v1.56.1/fakestorage/bucket.go) | `73ac2fded764470f13b451d281c61089704f48e79a0234cfdf164657d443cf96` |
| [gcp response/router](https://github.com/oittaa/gcp-storage-emulator/blob/v2026.07.19/src/gcp_storage_emulator/server.py) | `b0227517548e0c34304b06029712351de277bd2a8bb1c7c45e71d0a27ec55837` |
| [gcp object handlers](https://github.com/oittaa/gcp-storage-emulator/blob/v2026.07.19/src/gcp_storage_emulator/handlers/objects.py) | `e382f1715e55805a0b7459c1f9c8f21b8f38443da6c8d863ea6994bf1c1f4c4f` |
| [gcp bucket handlers](https://github.com/oittaa/gcp-storage-emulator/blob/v2026.07.19/src/gcp_storage_emulator/handlers/buckets.py) | `ce160c2cffdc84937d868a80ea91bed8141b5c2eae9cd623db228cf9fcfec01f` |

## Ledger and remaining scope

BEHAVIOR-COMMON-004 and BEHAVIOR-COMMON-012 are IMPLEMENTED with their unchanged
EXACT targets and EXACT achieved fidelity for the explicitly documented
selector and etag-bearing JSON GET scopes. Their 17 passing cases each have
linked final Rust/SDK evidence. Undocumented selector edges and provider wire
questions remain pending on the affected method rows and named gaps, following
the existing documented-range behavior precedent.

BEHAVIOR-OBJECT-006 and all 14 affected JSON method rows remain PARTIAL;
existing pending cases, broader permissions/fields and historical evidence are
preserved. SPEC-FIELDS-EDGES, SPEC-JSON-ETAG-WIRE and
SPEC-JSON-ETAG-COLLECTIONS record sources, observed local results and required
resolution; SPEC-012 no longer claims blanket rejection of read ETag headers.

Known limitations include empty/whitespace selectors, wildcard children and
fields plus raw media (501); local selector caps and wildcard/empty-parent/error
edges; provider ETag bytes, policy-header representation, complete headers and
mixed-extension ordering. Legacy bucket body etag can differ from the HTTP
validator, and same-millisecond equal-revision bucket incarnations can share a
tag. If-Range returns 501 even without Range. With an ETag request header,
gzip Accept-Encoding validation can precede a NotMatch 304; a query-only
NotMatch 304 retains the previous response without an ETag header. List/write
validators, JSON date conditions, nonexistent ACL routes and other protocols are
outside the delivered behavior scope. No real-cloud parity is inferred from
local tests or SDK success.

Only completion condition 2 of order 8 **Basic JSON contract** is delivered here.
Conditions 1, 3 and 4 remain open. The next slice stays in order 8: freeze the
complete basic local profile, reconcile existing atomic generation/metageneration
conditions, supplied MD5/CRC32C rejection and error ordering against
SPEC-008/009/011/012, then map each remaining basic gap to executed evidence or
explicit deferred scope before copy/rewrite/compose.
