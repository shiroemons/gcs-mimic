# Compatibility status and completion policy

## One authoritative ledger

`compatibility/features.json` owns every status. The canonical English table and optional Japanese companion table are derived views. Stable IDs never change merely because titles or priorities change. API operation rows, behavior rows and project requirements are different dimensions; never add them together and call the sum independent GCS features.

The initial kit was a planning baseline with every row unstarted. Current status is recorded only in the ledger. Documentation preparation does not count as API implementation. Fill acceptance cases and dependencies before changing a row to `IN_PROGRESS`.

| Status | Meaning |
| --- | --- |
| `NOT_STARTED` | No implementation claim |
| `IN_PROGRESS` | Work is active; no completed claim |
| `PARTIAL` | Some documented cases work; missing cases are explicit |
| `IMPLEMENTED` | All acceptance cases for the declared scope pass with evidence |
| `BLOCKED` | A concrete dependency or contract question prevents completion |
| `NOT_APPLICABLE` | Outside the declared product/profile; reason recorded and kept in the ledger |

| Fidelity | Meaning |
| --- | --- |
| `EXACT` | Observable contract reproduced for the explicitly tested local scope |
| `LOCAL_SIMULATION` | Time, infrastructure, analytics or service behavior represented by a deterministic local model |
| `METADATA_ONLY` | Field/config can be stored/read but its effects are not enforced; usually partial |
| `EXTERNAL` | Side effects require a separately provided service/integration |
| `PROJECT` | Emulator-specific usability/engineering requirement, not a Google API claim |
| `NONE` | No achieved fidelity yet |

`target_fidelity` is a plan. `actual_fidelity` must be established by tests. An implemented local simulation must remain visibly simulated; do not label it exact just because its local tests pass.

## Definition of done

### Capability delivery and broad feature rows

The ledger's optional `delivery_plan` records existing scoped evidence and the
ordered capability queue. It contains prerequisites, completion conditions and
exclusions, but no independent implementation statuses. Existing scope/case
references are navigation aids, not automatic completion of a delivery gate.

P0-P7 are coverage groups. A capability gate covers only its declared scope; a
clock-injection gate does not depend on later lifecycle scheduling, and a
single-request upload gate does not depend on resumable or gRPC support.
Before coding, map the gate to concrete acceptance cases on the affected feature
records. Keep known differences within that scope visible and unresolved until
evidence supports their resolution.

A completed narrow behavior does not complete its parent API method. Preserve
parent status, fidelity, pending cases and historical evidence. New requirements
extend a capability separately; regressions in an existing scope still require
repair. Do not narrow an existing feature row merely to remove its deviations.

Report completed scopes with evidence against a fixed selected delivery scope,
separately from method/behavior/project counts. Do not infer a delivery percentage
from planning metadata or treat overlapping feature references as independent
capabilities. README comparisons must use the same named scope for every product.

The JSON schema describes this planning metadata. The current Rust xtask still
validates feature records and generated views; it does not validate delivery-plan
references or execute its conditions. Review those references separately until
dedicated tooling is explicitly implemented.

### Migration and behavioral-superset claims

`delivery_plan.migration` records a target and completion conditions, with no
second implementation-status system. Legacy emulator parity is not Google
`EXACT` fidelity. Verify legacy behavior and strict permissions/signatures in
separate profiles; neither may silently override the other's contract.

Full replacement requires every declared source surface and lossless-data
case, including renamed configuration, TLS, helpers/events, Go embedding,
legacy Storage v1 gRPC and both backends. An early subset must name its limits.
Retain source identities and metadata; unsupported state blocks a lossless
migration claim. Documentation and source inspection are not passing evidence.

### Feature implementation gate

For the selected feature, record the following before marking it implemented:

1. The exact protocol/version, official source URL/date or source digest, relevant parameters/headers/fields, and supported profile.
2. Named acceptance cases for success, failure and boundary behavior; permission and feature-interaction cases where applicable.
3. Passing wire-contract tests and relevant SDK integration. For non-SDK project requirements, record the applicable CLI/container/doc check.
4. Atomicity, restart/crash, resource-use or timing evidence when the feature affects those properties.
5. Test commands, results and durable artifact paths; supported SDK versions/transports; unresolved deviations explicitly stated.
6. Updated canonical ledger and regenerated English documentation, plus requested companion views. A first commit can reference local evidence without embedding its own not-yet-existing hash; record the PR/commit later when known.

A completed method needs all its declared acceptance children, not one passing happy path. Implementation source links improve review but do not replace executable evidence. Unit tests that mirror internal code are insufficient for wire compatibility.

## Unsupported semantics

Recognized but unsupported semantics fail with **400 `badRequest`** in the JSON
API and **400 `InvalidArgument`** in the XML API. They never succeed silently.
Known unimplemented Google JSON method/path combinations use the same JSON
error instead of an ambiguous 404. Messages identify the setting or route and
suggest a supported call or removal of an option. This deliberate emulator
deviation avoids SDK retries of 501 and applies to both compatibility profiles.
Implemented operations on missing resources still return 404 `notFound`, and
unknown paths keep their existing handling. See the pinned route inventory,
acceptance and evidence in [unsupported SDK calls](slices/sdk-unsupported-fail-fast.md)
and SPEC-SDK-UNSUPPORTED in the gap ledger. This policy does not imply support
for the rejected feature or provider-exact rejection of a valid Google request.

## Schema and generated views

The Rust `xtask` validates unique IDs, enumerations, calendar dates, dependency references/cycles, counts, source provenance, fidelity and linked evidence. It generates `docs/en/feature-matrix.md` and `docs/ja/feature-matrix.md` deterministically, sorting categories/IDs and escaping table content. The companion title file must contain exactly the ledger IDs and no status. Kinds and achieved fidelity have separate summaries; SDK evidence counts remain per-row. Parameter/schema-field inventories are checked separately for counts, IDs, statuses and basic evidence presence. Detailed field semantics remain future conformance work.

Available commands (fetch the locked dependencies first):

```sh
cargo xtask compatibility validate
cargo xtask compatibility render
cargo xtask compatibility check
```

CI should fail when generated tables differ from the JSON. Report totals by surface, status and achieved fidelity. Keep methods, behavioral cases, fields and project requirements as separate denominators. An overall percentage is optional and should never be presented as a proof of full compatibility.

## A useful completed-row shape

Each acceptance case has a row-local unique `id`, `description`, `result` (`PENDING`, `PASS`, `FAIL`) and `test_ids`. Each verification has a nonempty `command`, `result` (`PASS`, `FAIL`), `verified_at` date, repository-relative `artifact` and `test_ids`. The row declares the complete test-ID set. A passing case must link to passing verification for all its tests.

`IMPLEMENTED` requires nonempty cases/tests/verification, every case and check passing, `acceptance_status: COMPLETE`, achieved fidelity equal to target, all tests linked, implemented dependencies, no deviations, and a valid `last_verified`. Artifacts must be nonempty files inside the repository, including after symlink resolution. `PARTIAL` requires explicit missing cases. Simulations cannot claim `EXACT`.

This repository-specific validator is not a general JSON Schema engine or proof that submitted logs are truthful. Reviewers verify outputs, coverage and SDK scope. It never runs commands from evidence records. Each JSON input is capped at 8 MiB. Validate/check are read-only and offline; render writes only the two tables. Edit the ledger explicitly. `implemented_in` may stay null until a real commit exists.

Each record includes `acceptance_cases`, `test_ids`, `verification`, `sdk_evidence`, `known_deviations`, `implemented_in`, and `last_verified`. They are intentionally empty now. Do not populate these with invented future results. Include output capture or CI links when they exist.

Cloud-dependent tests are opt-in. Local fixtures are not proof of real Google token verification, billing, KMS execution, BigQuery query behavior, Pub/Sub delivery guarantees or geographical durability. Record those boundaries honestly even when local state transitions are useful and complete.
