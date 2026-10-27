# Object temporary holds and custom time

<!-- markdownlint-configure-file {"MD013": false} -->

Scope: BEHAVIOR-OBJECT-035 and BEHAVIOR-OBJECT-004. Retention policies, lifecycle and soft delete remain pending.

## Contract and acceptance

| Case | Required behavior |
| --- | --- |
| TEMPORARY-HOLD-SET-RELEASE | Multipart/resumable upload, PATCH and PUT set/release temporaryHold; GET and restart retain it. False is omitted from responses; null means no change. |
| TEMPORARY-HOLD-PROTECTION | Shared event/temporary hold checks reject deletion and replacement by upload, copy, rewrite and compose with 403 objectUnderActiveHold; release permits mutation. |
| CUSTOM-TIME-SET | Upload, PATCH and PUT accept RFC 3339 customTime and return it; malformed values return 400. |
| CUSTOM-TIME-MONOTONIC | Equal/later instants succeed; earlier values and explicit null after setting fail atomically. PATCH omission preserves; PUT omission is rejected after setting customTime. |
| SDK-DEFAULT-CALLS | Go Writer/ObjectAttrsToUpdate and Ruby create_file, set_temporary_hold!, release_temporary_hold! and custom_time= work without transport workarounds. |

Permissions and generation/metageneration conditions retain existing transactional checks. Holds can coexist and either hold protects bytes. Replacing an object creates a new generation and can reset customTime, as documented for rewrite.

## Sources and local decisions

Checked 2026-10-27: [object resource](https://docs.cloud.google.com/storage/docs/json_api/v1/objects), [object update](https://docs.cloud.google.com/storage/docs/json_api/v1/objects/update), [object holds](https://docs.cloud.google.com/storage/docs/object-holds), [metadata](https://docs.cloud.google.com/storage/docs/metadata#custom-time), [JSON errors](https://docs.cloud.google.com/storage/docs/json_api/v1/status-codes) and [Discovery](https://storage.googleapis.com/$discovery/rest?version=v1).
Discovery revision `20261020`, SHA-256 `eeb0831df501d131c9497534b2c3d130ea40f9fa215195115d011cb64e1a42bc` (same pinned input as sdk-ruby-defaults).
The references establish writable boolean holds, non-removable/non-decreasing customTime and 403 objectUnderActiveHold. The update method explicitly requires an equal/later customTime on PUT when it is already set; PATCH changes only supplied fields. They do not establish explicit-null hold handling or exact customTime failure messages/reasons. SPEC-OBJECT-HOLDS-CUSTOMTIME records those local wire choices: temporaryHold:null is absent; clearing, backdating or omitting customTime from PUT after setting returns 400 invalidArgument with corrective guidance. Provider differential evidence and combined error precedence remain pending.

The catalog stores customTime as validated RFC 3339 text, retaining offsets and up to nine fractional digits. Comparison uses the parsed instant, including nanoseconds; no payload buffering or additional catalog scan is introduced. Leap seconds and years outside 0001–9999 are rejected locally. XML PUT/delete share the hold protection, with the existing 403 AccessDenied mapping. XML custom-time headers, new retention/lifecycle interactions and source-field inheritance for derived objects are outside this slice. Setting temporaryHold:true or a valid non-null customTime on copy/rewrite/compose remains 501; false/null no-op defaults are accepted. Malformed customTime returns 400 invalidArgument before unsupported semantics are checked. These operations still reject replacing an already-held destination.
Schema 22 adds a false-default temporary_hold column and nullable custom_time in one transaction, preserving existing objects. Persisted resumable options retain the new fields, including sessions resumed after restart. Existing version-1/2 options lacking those fields remain readable.

The pinned Go SDK formats Writer customTime using time.RFC3339 (whole seconds); the SDK test uses whole seconds while wire tests cover nanosecond boundaries. Ruby's documented custom_time input is DateTime; the tests use DateTime.new and ordinary create_file/update calls.

## Initial implementation verification

Acceptance was recorded before implementation. The original server rejected Go Writer temporaryHold and Ruby create_file(custom_time:) with 501. No provider requests were made.

- `cargo check --workspace --all-targets --locked --offline`: PASS.
- `cargo test --locked --offline --test object_holds_customtime_http --test object_metadata_http --test object_versioning_http`: PASS (5, 5 and 9 cases), including PUT omission rejection and unchanged data on failures.
- Existing catalog checks: PASS (21 cases). The complete Rust test inventory passes across the workspace run and resumed integration targets, including library 464, object hold 4, copy 9, compose 18, multipart 21, rewrite 25 and xtask 37 cases. The final derived-metadata unit and doc-test checks also pass.
- `BUNDLE_FROZEN=true bundle check` using Ruby 4.0.9 / Bundler 4.0.23: PASS.
- `just check-sdk`: PASS. Complete Go vet/suite, Ruby origin self-check and migration scenario, and Ruby permissive/strict suites pass; both runs print ruby-object-holds-custom-time passed. That execution used Go 1.27.3, Storage v1.70.0, API client v0.308.0 and Ruby google-cloud-storage 1.62.1.
- `just check`: one invocation exited 101 on five stale schema fixtures (459 library cases passed). After correcting those fixtures and updating old unsupported-field assertions, each remaining check stage was completed separately: final cargo check/clippy, Rust test inventory, compatibility check, offline release-script checks and syntax checks for all 20 shell scripts PASS. The recipe itself was not rerun. Host and both cross-target clippy lanes passed during that invocation; final host clippy also passes after the corrections.
- `npx --yes markdownlint-cli2 docs/en/slices/sdk-object-holds-customtime.md docs/en/fake-gcs-server-migration.md`: PASS (0 issues).
- Containers: not run (out of scope).

The slice-start dependency check found Rust 1.99.1 and direct Rust dependencies current. The API client v0.309.0 and SBOM scanner updates found then were subsequently merged on main before the rebase. This slice adds no dependencies or lockfile changes. The preceding defaults slice has no Japanese slice companion, so this slice follows that layout; both generated feature matrices use the ledger.

Both feature rows remain PARTIAL: the scoped local cases pass, while complete provider error/null parity and retention interactions remain PENDING. Benchmarks and real-cloud differential tests were not run. Full guide journeys were not rerun because no runnable guide commands changed.

## Rebase verification

Rebased onto main `8d98358`, preserving listing globs, dependency updates and bucket testIamPermissions. The migration guide and Ruby harness retain both tracks' cases; generated English/Japanese matrices were regenerated from the merged ledger.

- `cargo check --workspace --all-targets --locked --offline`: PASS.
- HTTP suites object_copy_http, object_compose_http, rewrite_http, object_hold_http, object_holds_customtime_http, object_multipart_http, object_metadata_http and object_versioning_http: PASS (10, 19, 26, 4, 6, 21, 5 and 9 cases; 100 total). New cases verify false/null destination defaults separately on copy, rewrite and compose, valid non-null rewrite customTime rejection, and malformed customTime rejection before 501 on all three routes without changing existing data.
- `cargo test --locked --offline --lib rejects_future_schema_unversioned_data_and_corruption`: PASS (1 case).
- `cargo xtask compatibility check`: PASS; merged ledger and both generated matrices match.
- Ruby harness syntax: PASS. Full SDK and just check were not rerun; integration-wide execution belongs to the main merge verification.
