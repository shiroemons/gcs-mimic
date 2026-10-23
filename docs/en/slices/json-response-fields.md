# JSON partial response field selection

<!-- markdownlint-configure-file {"MD013": false} -->

Checked and verified on **2026-10-23**. Implementation commit: `0f01ee5`.
This slice supplies the supported-resource subset of `BEHAVIOR-COMMON-004`.
Integration should record the broader behavior as `PARTIAL` and preserve method
statuses while the named edge contracts below remain unresolved. The integration
task owns the feature/gap ledgers and generated matrices; this slice changes none
of those shared files.

## Scope

Apply `fields` to the already-rendered successful JSON representation of:

- `buckets.get`, `buckets.insert`, `buckets.list`, `buckets.patch`, `buckets.update`;
- `objects.get` metadata (`alt=json` or omitted), media/multipart/resumable
  `objects.insert`, `objects.list`, `objects.patch`, `objects.update`;
- bucket `getIamPolicy` and `setIamPolicy`.

Selection does not alter authorization, preconditions, uploaded bytes or stored
metadata. Mutations commit their complete supported input even when the returned
resource contains one field. Error envelopes never enter the mask renderer.
DELETE ignores the selector, including an empty or malformed selector, and
retains the existing response behavior. General query decoding, duplicate-query
and 16 KiB query limits still apply. XML adapters are unchanged.

For resumable insertion, initiation validates and persists the selector as an
optional string alongside the existing upload options. Initiation still has an
empty response body. Finalization renders and filters the completed object;
the existing durable completion snapshot preserves that selected response
across retries and restart. Older options without this optional property decode
as no selection; the catalog schema version is unchanged.

## Sources and pins

All sources below were checked on **2026-10-23**. SHA-256 values identify fetched
bytes without normalization. No real-cloud resource operation was performed.

| Source | Contract used | SHA-256 or immutable version |
| --- | --- | --- |
| [Cloud Storage partial response](https://docs.cloud.google.com/storage/docs/json_api/v1/how-tos/performance#partial-response), now redirecting to [JSON API overview](https://docs.cloud.google.com/storage/docs/json_api#partial-response) | Comma lists, slash paths, sub-selectors, arrays, enclosing parents, omitted custom metadata, response-only effect and invalid-selection HTTP 400 | `319dcc2870be38eb010a0c9645ed3f321aa99528b5d73a80ce666619f696ba3e` |
| [Standard JSON parameters](https://docs.cloud.google.com/storage/docs/json_api/v1/parameters) | `fields` selects response fields; DELETE has no response fields to select | `b07847abf567188a524f59281f1206e00c9c8c54683bff39dd361bb50e240664` |
| [Google partial-response syntax](https://developers.google.com/workspace/drive/api/guides/fields-parameter) | Corroborates shared slash/parenthesis syntax and terminal `*` wildcard; Drive-specific defaults are not adopted | `334e894c41548d7bbafeaa4e09cfe8ac019df58855ff0890b3a64cb31ea70bd1` |
| [Live JSON Discovery](https://storage.googleapis.com/$discovery/rest?version=v1) | Allowed response properties, `$ref`, array items and dynamic map properties | Revision `20261013`; `1f74b7a93b3d2872cc332fc20dafae1657fc255930ce336618bea3d0b4d95ae0` |
| [JSON status codes](https://docs.cloud.google.com/storage/docs/json_api/v1/status-codes) | Invalid URL parameter values use `invalidParameter` | `45273942c7f4fea45ed9d27037308698fc4cf0188d3084f41a43b358fb75ddee` |
| [Pinned Go Storage source](https://github.com/googleapis/google-cloud-go/tree/storage/v1.70.0/storage) and [generated JSON API source](https://github.com/googleapis/google-api-go-client/blob/v0.308.0/storage/v1/storage-gen.go) | `Query.SetAttrSelection` and generated `Buckets.Get(...).Fields(...)` | Storage `v1.70.0`, API `v0.308.0`, Go `1.27.3`; existing `go.mod`/`go.sum` unchanged |

Runtime schema validation reuses the factual extract in
`compatibility/upstream/json-schemas.json`, SHA-256
`46c23b96df1d02c6303f3aef8cefe5bbe6b9729b7ac6065be9745a377e084f2c`.
After recursively removing prose `description` and `enumDescriptions` from the
live Discovery schemas, the schema objects compare identically to this pin.
No protobuf field-mask contract is substituted for this legacy JSON query syntax.
The Google API `data` wrapper exception is irrelevant to these unwrapped Storage
representations; `data` is not an extra root field.

Malformed selectors and unknown schema properties return **400
`invalidParameter`**. The Storage partial-response page establishes HTTP 400 and
an invalid-selection message; the status-code reference establishes this reason
for an invalid URL parameter. The exact service message, error location fields
and precedence between multiple invalid inputs were not differentially verified.

## Supported subset and resource bounds

- Comma lists, repeated/overlapping selections, `a/b/c`, `items(name,size)` and
  nested parentheses are supported. Whole-parent selection wins over a narrower
  selection of that same parent; all selected children are still validated.
- A terminal `*` selects every existing property at that level: `*`, `items(*)`
  and `items/metadata/*` work. A wildcard followed by a path or sub-selector is
  explicitly unsupported, including when combined with a duplicate whole wildcard.
- Validation uses response schemas, rather than the fields present in one
  resource, so an absent optional property is legal. An unknown property or a
  child of a scalar is invalid even on an empty list. Dynamic labels/metadata
  keys are accepted at their schema-defined map level.
- Filtering only removes values. Absent properties stay absent and are never
  synthesized as `null`; existing arrays preserve order and element count.
  Selecting a missing map key can leave an empty enclosing map. Fields already
  omitted by the existing supported resource renderer remain omitted; selecting
  them does not implement ACL, owner or other unmodeled semantics.
- The decoded selector is at most **4096 UTF-8 bytes**, **32 path/parenthesis
  levels** and **256 parsed field segments**, including duplicates and wildcards.
  Exceeding a cap returns 400 `invalidParameter` before mutation. These are
  explicit emulator resource limits, not claims about Google service caps.
- Parsing and pruning remain in the JSON adapter. The pinned schema is parsed
  once through `LazyLock`; there are no new dependencies or network accesses.
  The Docker build copies only that required factual schema into the build stage.

## Acceptance cases

Rust integration-test paths below use `target::function`; the target is the
integration test filename without `.rs`. PASS means the declared local behavior
was executed, not that unestablished provider edges were inferred.

| Case | Description | Test IDs | Result |
| --- | --- | --- | --- |
| FIELDS-001 | Comma lists, equivalent slash paths, parentheses and nested sub-selection preserve enclosing parents | `json::fields::tests::fields_select_paths_subselectors_wildcards_and_omitted_values` | PASS |
| FIELDS-002 | Root/nested terminal wildcards and overlapping selections retain all selected values | `json::fields::tests::fields_select_paths_subselectors_wildcards_and_omitted_values` | PASS |
| FIELDS-003 | Unknown names, wrong collection roots, scalar children, unbalanced parentheses, trailing commas and empty segments fail with 400 `invalidParameter` | `json::fields::tests::fields_reject_malformed_unknown_and_scalar_children`; `fields_http::fields_invalid_selectors_and_errors_never_filter_or_mutate` | PASS |
| FIELDS-004 | Exact/over-limit length, nesting and segment counts are bounded | `json::fields::tests::fields_bound_length_depth_and_selection_count` | PASS |
| FIELDS-005 | All five bucket metadata methods, including committed PATCH/PUT labels, select response fields | `fields_http::fields_bucket_methods_and_policy_preserve_mutations` | PASS |
| FIELDS-006 | Bucket lists select kind/items/token; a selected token continues paging while the next response can select different fields | `fields_http::fields_bucket_methods_and_policy_preserve_mutations` | PASS |
| FIELDS-007 | Bucket IAM GET/PUT sub-selection filters bindings without changing stored policy members | `fields_http::fields_bucket_methods_and_policy_preserve_mutations` | PASS |
| FIELDS-008 | Object metadata GET/PATCH/PUT and media insertion preserve the complete stored mutation, including zero-byte size `"0"` | `fields_http::fields_object_methods_lists_and_zero_byte_metadata` | PASS |
| FIELDS-009 | Object lists select kind/items/prefixes/token; selected tokens retain pagination and can be omitted later | `fields_http::fields_object_methods_lists_and_zero_byte_metadata`; `json::fields::tests::fields_select_paths_subselectors_wildcards_and_omitted_values` | PASS |
| FIELDS-010 | Missing optional properties remain absent, including on zero-byte objects; filtering does not create nulls | `json::fields::tests::fields_select_paths_subselectors_wildcards_and_omitted_values`; `fields_http::fields_object_methods_lists_and_zero_byte_metadata` | PASS |
| FIELDS-011 | Multipart insertion filters its result while preserving unselected metadata and exact stored bytes | `object_multipart_http::fields_multipart_response_preserves_uploaded_metadata_and_bytes` | PASS |
| FIELDS-012 | Resumable initiation has no JSON body; selection survives restart before completion and replay after another restart | `fields_http::fields_resumable_final_response_survives_restart_and_replay` | PASS |
| FIELDS-013 | Strict denied requests and missing/condition/credential errors retain complete 403/404/412/401 error envelopes; invalid selectors do not mutate | `fields_http::fields_invalid_selectors_and_errors_never_filter_or_mutate` | PASS |
| FIELDS-014 | DELETE ignores fields and keeps empty 204; ordinary JSON media and XML reads remain intact; fields plus alt=media explicitly fails 501 | `fields_http::fields_delete_is_ignored_media_is_explicitly_unsupported_and_xml_unchanged` | PASS |
| FIELDS-015 | Empty/whitespace selectors and wildcard children explicitly fail 501 pending contract resolution | `json::fields::tests::fields_reject_malformed_unknown_and_scalar_children`; `fields_http::fields_invalid_selectors_and_errors_never_filter_or_mutate` | PASS |
| FIELDS-016 | Pinned Go generated bucket fields and high-level object iterator attribute selection return selected attributes | `TestPartialResponseFields` | PASS |
| FIELDS-017 | Provider behavior for empty/whitespace selectors, wildcard children, media combination, exact error edges and local caps | No authorized provider fixture; named gap below | PENDING |
| FIELDS-018 | Combined read ETag validator and fields interaction after the independent validator slice is integrated | Integration task; baseline metadata GET emits no ETag response header | PENDING |

## Explicit exclusions and named gaps

**SPEC-FIELDS-EDGES**: The checked Storage documentation does not establish empty
selector meaning, whitespace normalization, wildcard child selection or how
`fields` combines with raw `alt=media` responses. These remain explicit 501
`notImplemented` failures, rather than silently ignored settings. The wildcard
child restriction also holds for overlapping selectors. The exact service error
message/location, combined-invalid-input precedence and service limits remain
unverified; local 400 `invalidParameter` mapping and the stated caps are explicit.
Obtain authoritative clarification or an authorized provider fixture before
changing those behaviors or claiming full partial-response parity.

The returned resumable capability URL does not accept a new `fields` override;
selection belongs to the insertion request. The current session route rejects
additional query parameters with 400. Provider override behavior is unestablished
and belongs to the same gap. General cancellation/upload protocol behavior stays
under the existing resumable contract.

The independent ETag slice must compare the complete resource's ETag
before pruning its body. It must preserve ETag/cache headers and empty 304/412
responses even when `fields` excludes `etag`. This branch's hooks are immediately
before JSON serialization; integration must carry the same mask to any new early
metadata-success return path. No ETag-header interaction is claimed here.

Full method compatibility, new resource fields, XML partial response, gRPC,
container execution, the full Go suite, Ruby SDKs, real-cloud differential tests,
performance measurements and getting-started journeys are outside this worker's
verification. Source-built containers and combined validators are integration
checks, not completed evidence from this slice.

## Ledger updates for integration

Add the acceptance cases above to `BEHAVIOR-COMMON-004`; set it from `NOT_STARTED`
to `PARTIAL`, record the exact supported subset and retain FIELDS-017/018 pending.
Add SPEC-FIELDS-EDGES to the gap ledger with the sources, local rejections and
resolution requirement above. Preserve each method's existing status, cases,
evidence, fidelity and unrelated gaps. Replace old notes that reject every
`fields` value with the supported subset and remaining exceptions.

| Feature rows | Acceptance cases to add |
| --- | --- |
| `JSON-BUCKETS-002`, `JSON-BUCKETS-005`, `JSON-BUCKETS-008`, `JSON-BUCKETS-013` | FIELDS-001/003/004/005/010/013/015; bucket GET additionally FIELDS-018 pending |
| `JSON-BUCKETS-006` | FIELDS-001/002/003/004/006/010/015/016 |
| `JSON-BUCKETS-003`, `JSON-BUCKETS-011` | FIELDS-001/003/004/007/013/015 |
| `JSON-OBJECTS-005` | FIELDS-001/002/003/004/008/010/013/014/015; FIELDS-018 pending |
| `JSON-OBJECTS-007` | FIELDS-001/003/004/008/010/011/012/013/015 |
| `JSON-OBJECTS-008` | FIELDS-001/002/003/004/009/010/015/016 |
| `JSON-OBJECTS-010`, `JSON-OBJECTS-015` | FIELDS-001/003/004/008/010/013/015 |
| `JSON-BUCKETS-001`, `JSON-OBJECTS-004` | FIELDS-014 (fields ignored; existing delete semantics retained) |

Exact new Rust test IDs are the three `json::fields::tests::*` paths, five
`fields_http::*` paths and one `object_multipart_http::*` path listed in the
acceptance table. The exact Go test is `TestPartialResponseFields` in
`sdk-tests/go/fields_test.go`. Existing negative-test coverage for unsupported
parameters remains: only formerly unsupported valid field selections were
replaced by the still-unsupported empty selector, while false/unknown object
field names now assert 400. DELETE's former empty-fields rejection now asserts
the documented ignored-parameter behavior.

Commands executed from the repository root on 2026-10-23:

| Command | Result |
| --- | --- |
| `cargo +1.99.1 test --lib --locked --offline fields` | PASS; seven matching tests, including the three new parser/pruner cases |
| `cargo +1.99.1 test --locked --offline --test fields_http --test object_multipart_http fields` | PASS; five new general HTTP cases and two matching multipart cases, including the new multipart selection case |
| `cargo +1.99.1 test --workspace --locked --offline` | PASS; all workspace unit/integration/doc tests |
| `cargo +1.99.1 test --workspace --locked --offline --quiet` | PASS on final source, including the overlapping wildcard-child rejection |
| `cargo +1.99.1 fmt --all --check` | PASS |
| `cargo +1.99.1 clippy --workspace --all-targets --locked --offline -- -D warnings` | PASS |
| `GCS_MIMIC_GO_TEST_RUN=TestPartialResponseFields GOPROXY=off GOFLAGS=-mod=readonly bash scripts/sdk-go.sh` | PASS; Go vet and exactly the focused SDK test; Storage v1.70.0/API v0.308.0, no new Go dependencies |
| `cargo +1.99.1 xtask dependencies check` | PASS; all 31 monitored compiler/crate/SDK/container/CI inputs CURRENT |
| `npx --yes markdownlint-cli2 docs/en/slices/json-response-fields.md` | PASS; zero issues; MD013 disabled for the acceptance/evidence tables as in neighboring canonical slice documents |
| `git diff --check` and `git diff --staged --check` | PASS |
| Local-path scan using the requested Git search expression | PASS; no matches before implementation commit |

The online dependency check was read-only and did not change pins or ledgers.
All default Rust and focused SDK tests used isolated local emulators; no Google
Cloud or metadata-server endpoint was contacted by those tests.
