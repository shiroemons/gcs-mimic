# Compatibility status and completion policy

## One authoritative ledger

`compatibility/features.json` owns every status. The canonical English table and optional Japanese companion table are derived views. Stable IDs never change merely because titles or priorities change. API operation rows, behavior rows and project requirements are different dimensions; never add them together and call the sum independent GCS features.

The initial kit was a planning baseline with every row unstarted. Current status is recorded only in the ledger. Documentation preparation does not count as API implementation. Fill acceptance cases and dependencies before changing a row to `IN_PROGRESS`.

## Product profile

gcs-mimic is an emulator for the official Cloud Storage SDKs. The
[roadmap's scope](roadmap.md#scope) lists what is in and out of the product.
A row outside it is `NOT_APPLICABLE`; it returns to `NOT_STARTED` when an
official SDK workflow needs it, with the reason recorded in its notes.

Behavior is decided by what the version-pinned official SDKs send and accept,
then by Google documentation and the googleapis protos. Nothing in the
repository contacts Cloud Storage, so no row waits for or cites new
real-service evidence; see
[no live Cloud Storage contact](development.md#no-live-cloud-storage-contact).
Earlier rows that cite recorded Cloud Storage answers keep those dated
records.

Historical evidence, past release notes and recorded verification commands retain the environment names used at the time; current non-server variables use `GCSM_*`, as listed in the [environment migration table](releases/0.18.0.md#environment-variable-names). The documentation check permits only the explicit historical file/suffix counts in `scripts/environment-history-baseline.txt`, so a new record or an increased count still fails.

| Status | Meaning |
| --- | --- |
| `NOT_STARTED` | No implementation claim |
| `IN_PROGRESS` | Work is active; no completed claim |
| `PARTIAL` | Some documented cases work; missing cases are explicit |
| `IMPLEMENTED` | All acceptance cases for the declared scope pass with evidence |
| `BLOCKED` | A concrete dependency or contract question prevents completion |
| `NOT_APPLICABLE` | Outside the [product profile](#product-profile); the row's notes give the date and reason, and the row stays in the ledger |

| Fidelity | Meaning |
| --- | --- |
| `EXACT` | Observable contract reproduced for the explicitly tested local scope |
| `LOCAL_SIMULATION` | Time, infrastructure, analytics or service behavior represented by a deterministic local model |
| `METADATA_ONLY` | Field/config can be stored/read but its effects are not enforced; usually partial |
| `EXTERNAL` | Side effects require a separately provided service/integration |
| `PROJECT` | Emulator-specific usability/engineering requirement, not a Google API claim |
| `NONE` | No achieved fidelity yet |

`target_fidelity` is a plan. `actual_fidelity` must be established by tests. An implemented local simulation must remain visibly simulated; do not label it exact just because its local tests pass.

### Choosing the target

A row targets `LOCAL_SIMULATION` when the behavior it tracks is produced by
Google infrastructure that the emulator replaces with a deterministic local
model. No test can then establish `EXACT`, so an `EXACT` target would keep the
row `PARTIAL` however complete its local cases are. A row whose behavior is a
request and response contract targets `EXACT`, even when it shares state with a
simulated mechanism or runs under local identities: the simulation is tracked by
its own row, not inherited by every API row that touches it. Such a row has
`EXACT` achieved fidelity for its tested subset while unfinished cases keep it
`PARTIAL`.

Applied on 2026-11-13:

| Rows | Target | Achieved | Reason |
| --- | --- | --- | --- |
| BEHAVIOR-LIFECYCLE-002 to 009 | `LOCAL_SIMULATION` | `LOCAL_SIMULATION` | A local sweep applies lifecycle actions at locally chosen times. Cloud Storage runs them asynchronously without a timing guarantee and propagates configuration for up to 24 hours. Condition truth and transitions are tested, but when they take effect is the local model. BEHAVIOR-LIFECYCLE-010 already tracks the scheduler itself. |
| EXT-BEH-032 | `LOCAL_SIMULATION` | `LOCAL_SIMULATION` | A local outbox publishes events to an explicitly configured Pub/Sub emulator in place of Cloud Storage's notification service. |
| DESIGN-002, DESIGN-004 | `LOCAL_SIMULATION` | `LOCAL_SIMULATION` | Callers are registered local identities; Google OAuth tokens and the Google IAM directory are not consulted. |
| BEHAVIOR-LIFECYCLE-001 | `EXACT` | `EXACT` | Lifecycle configuration validation is a request contract. |
| EXT-XML-004, 011, 013, 019, 020, 023 | `EXACT` | `EXACT` | XML multipart operations are request and response contracts; the seven-day session expiry is a recorded local bound, not a simulation. |
| JSON-OBJECTS-004, 005, 007, 008, 010, 015 | `EXACT` | `EXACT` | Object methods are request and response contracts; strict local identities belong to DESIGN-002. |

All of these rows stay `PARTIAL` until their known deviations are resolved.

## Definition of done

### Broad feature rows

P0-P7 are coverage groups, not execution gates. Before coding, map a slice to
concrete acceptance cases on the affected feature records. Keep known
differences within that scope visible and unresolved until evidence supports
their resolution.

A completed narrow behavior does not complete its parent API method. Preserve
parent status, fidelity, pending cases and historical evidence. New requirements
extend a capability separately; regressions in an existing scope still require
repair. Do not narrow an existing feature row merely to remove its deviations.

`known_deviations` lists missing or unverified work and blocks `IMPLEMENTED`.
`scoped_differences` lists intentional local contracts that are already
enforced: an explicit rejection of an out-of-scope feature, a documented local
bound, or a local choice where official sources are silent. Each entry names the
`compatibility/gaps.json` entry that records it, and that entry must list the
row in `affected_features`. Each entry also links passing tests that pin the
local behavior. Moving an item from `known_deviations` to `scoped_differences`
is a scope decision recorded in the gap ledger, not a deletion: an item without
an enforcing test, or one whose code still lacks the behavior, stays a known
deviation. When a moved item's gap entry says to keep the row `PARTIAL`, update
that `required_resolution` in the same change.

Real-service evidence is not collected, and its absence never keeps an `EXACT`
row `PARTIAL`. `EXACT` means the observable contract is reproduced for the
explicitly tested local scope; open provider questions belong in the gap ledger
as `SOURCE_QUESTION` entries and in the row as scoped differences. A row still
stays `PARTIAL` while its local behavior is untested, unimplemented or known to
differ from documented Google behavior.

Older gap entries whose `required_resolution` asks for differential, recorded
or real-service evidence are read the same way: only Google documentation, the
googleapis sources or the pinned official SDKs can resolve them, and no entry
authorizes contacting Cloud Storage.

Report completed scopes with evidence, separately from method/behavior/project
counts. Do not treat overlapping feature references as independent
capabilities. README comparisons must use the same named scope for every product.

### Migration claims

Legacy emulator parity is not Google `EXACT` fidelity. Verify legacy behavior
and strict permissions/signatures in separate profiles; neither may silently
override the other's contract.

gcs-mimic does not claim to replace fake-gcs-server completely. The
[migration guide](fake-gcs-server-migration.md) names the supported workflows
and their limits. Retain source identities and metadata; unsupported state
blocks a lossless migration claim. Documentation and source inspection are not
passing evidence.

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
One exception follows Cloud Storage: a JSON multi-range media request answers
501 (an XML multi-range GET answers 400).
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

A test ID made of identifier characters names a test function, optionally qualified as `module::path::name` or as a Go `TestName/Subtest`, and `cargo xtask compatibility` fails when the code holds no such name. Any other ID, such as `guide-native-object-media`, labels a script or manual lane that its verification's `command` describes. Renaming or merging a test therefore updates the ledger in the same change, and a replacement gets a new verification record rather than a rewritten past one.

A failed verification stays in the record. After a fix, add the passing rerun as a new record after it and give the failed record `superseded_by` with that rerun's `command`, `verified_at` and a nonempty `reason`. The rerun must pass, be dated no earlier than the failure, appear later in the list, match exactly one record and cover every failed test ID. A superseded failure no longer blocks `IMPLEMENTED`; an unsuperseded one does.

`IMPLEMENTED` requires nonempty cases/tests/verification, every case passing, every check passing or superseded, `acceptance_status: COMPLETE`, achieved fidelity equal to target, all tests linked, implemented dependencies, no known deviations, valid scoped differences, and a valid `last_verified`. A row's own `known_deviations` never restates an incomplete dependency: the validator already blocks `IMPLEMENTED` until every dependency is `IMPLEMENTED`. Artifacts must be nonempty files inside the repository, including after symlink resolution. `PARTIAL` requires explicit missing cases: a known deviation or an incomplete dependency. Simulations cannot claim `EXACT`.

This repository-specific validator is not a general JSON Schema engine or proof that submitted logs are truthful. Reviewers verify outputs, coverage and SDK scope. It never runs commands from evidence records. Each JSON input is capped at 8 MiB. Validate/check are read-only and offline; render writes only the two tables. Edit the ledger explicitly. `implemented_in` may stay null until a real commit exists.

Each record includes `acceptance_cases`, `test_ids`, `verification`, `sdk_evidence`, `known_deviations`, `implemented_in`, and `last_verified`. Rows without completed work keep them empty. Do not populate these with invented future results. Include output capture or CI links when they exist.

The repository has no cloud-dependent tests. Local fixtures are not proof of real Google token verification, billing, KMS execution, BigQuery query behavior, Pub/Sub delivery guarantees or geographical durability. Record those boundaries honestly even when local state transitions are useful and complete.
