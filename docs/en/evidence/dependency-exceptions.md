# Dependency freshness exceptions verification

<!-- markdownlint-configure-file {"MD013": false} -->

Scope: DEP-007, the expiring freshness exceptions that `cargo xtask dependencies check` reads from the `exceptions` list of [compatibility/dependency-status.json](../../../compatibility/dependency-status.json). Verified on 2026-11-19 on macOS (Apple Silicon) with the pinned Rust toolchain, at commit `b92475e8`. The record covers the unit tests and the local gates only; the live check was not run (see "Not yet verified").

The record schema, the input key, the status lines and the exit codes are defined once, in [How the freshness check applies an exception](../dependency-policy.md#how-the-freshness-check-applies-an-exception). This record does not restate them. The implementation is in `xtask/src/dependencies/exceptions.rs`, applied by `xtask/src/dependencies.rs`; the tests are in `xtask/src/dependencies/exceptions/tests.rs`.

## Acceptance cases and tests

All tests are in `dependencies::exceptions::tests`. They drive the report with fixed inputs and a fixed date, so they establish the decision for each input, the printed lines and the pass/fail flag, not the process exit status.

| Case | Test | Behavior |
| --- | --- | --- |
| DEP-007-A1, DEP-007-A2 | `an_in_date_exception_defers_its_update_and_the_full_check_passes` | With two manifests recorded, each reports `DEFERRED` with `current`, `latest`, `review_by` and its own `manifest` and does not fail; another input of the same manifest still reports `UPDATE_AVAILABLE` and fails; the run ends with the `DEFERRED:` summary (count, earliest `review_by`, "dependencies are not all current") |
| DEP-007-A3 | `the_exception_holds_through_its_review_date_and_fails_the_day_after` | Defers on 2026-11-25 and on the review date 2026-11-26; on 2026-11-27 prints `UPDATE_AVAILABLE` and `EXCEPTION_EXPIRED` and fails |
| DEP-007-A4 | `an_upstream_release_past_the_recorded_latest_fails` | A newer release prints `UPDATE_AVAILABLE` and `EXCEPTION_OUTDATED` and fails |
| DEP-007-A5 | `a_pin_other_than_the_recorded_current_fails` | A different pin prints `UPDATE_AVAILABLE` and `EXCEPTION_OUTDATED` and fails |
| DEP-007-A6 | `every_reason_a_lapsed_exception_no_longer_holds_is_reported` | A changed pin, a newer release and an expired date together print two `EXCEPTION_OUTDATED` lines and one `EXCEPTION_EXPIRED` line |
| DEP-007-A7 | `an_exception_for_a_current_input_is_stale` | `CURRENT` and a moving image tag `CURRENT` on its declared platforms each print `EXCEPTION_STALE` and fail |
| DEP-007-A8 | `an_exception_that_matches_no_input_fails` | A record whose manifest names no checked input leaves the input's usual line and fails at the end with `EXCEPTION_STALE` |
| DEP-007-A9 | `two_exceptions_for_one_input_fail` | Two records for one input and manifest, and a manifest listed twice in one record, are rejected; the same label in another manifest parses |
| DEP-007-A10 | `each_missing_unknown_or_empty_field_is_named` | For each of the eleven fields: missing, empty, whitespace-only and empty-list values are rejected with the record and field named; a non-string value, an unknown field, a non-object record and a non-list `exceptions` value are rejected |
| DEP-007-A11 | `a_review_date_must_be_a_calendar_date` | `2026-11-31`, `2026/11/26`, `26-11-26`, `next week` and a timestamp are rejected |
| DEP-007-A12 | `evidence_must_be_a_file_in_the_repository` | A missing file, a directory, an absolute path, a path with `..` and an empty path before the fragment are rejected; a path without a fragment parses |
| DEP-007-A13 | `release_scope_never_defers_a_release_input` | Under the release scope an update to a release input prints `EXCEPTION_REFUSED` and fails, the same input stays `CURRENT` when current, and the exception of the skipped development-only input is not reported |
| DEP-007-A14 | `without_exceptions_every_input_is_reported_as_before` | In both scopes the four input outcomes print as before and the run adds no line |
| DEP-007-A15 | `a_deferred_moving_image_tag_names_its_platforms` | The `DEFERRED` line of an image tag keeps `on linux/amd64` |
| DEP-007-A16 | `committed_exceptions_parse_and_name_manifests_the_check_reports` | The committed `exceptions` parse and each manifest is one the check reports |
| DEP-007-P1 | `freshness-live-deferred`, the live run recorded below | The full-scope check reports the recorded Temurin deferral as `DEFERRED` for both Java manifests and exits 0 |

## Commands and results

| Command | Result |
| --- | --- |
| `just check` at `b92475e8` | PASS, exit 0 on 2026-11-19: `cargo test` for the `xtask` package with 295 library unit tests passed and 0 failed, plus 13 `compatibility`, 3 `dependencies` and 1 `differential` tests; host `cargo xtask lint` with 0 violations; `cargo xtask lint --target` clean for `x86_64-pc-windows-msvc`, `x86_64-unknown-linux-gnu` and `x86_64-unknown-linux-musl` |
| `cargo test -p xtask` | PASS on 2026-11-19, rerun for this record: 295 library unit tests (including the 15 above), 13 `compatibility`, 3 `dependencies` and 1 `differential` tests, 0 failed |
| `cargo xtask dependencies check` at `2026-11-19T05:05:09Z`, with `GITHUB_TOKEN` set | PASS, exit 0: 142 `CURRENT` lines; `Temurin toolchain: DEFERRED current=27.0.1.0.2+1 latest=27.0.1.1+4 review_by=2026-11-26` once for `manifest=sdk-tests/java/pom.xml` and once for `manifest=sdk-scorecard/java/pom.xml`; the last line reads `DEFERRED: 2 updates deferred by recorded exceptions, earliest review_by 2026-11-26; dependencies are not all current`; fake-gcs-server v1.56.1 stays `PINNED_REFERENCE`. A run four minutes earlier stopped with `CHECK_FAILED` and exit 2 on a 30-second timeout fetching the Go module proxy, before it reached the Java inputs |

## Not yet verified

- No automated test runs the command itself, so the process exit status of a lapsed exception (1) and of a malformed record (2) is not asserted; the live run above observed only the deferred path and exit 0.
- That the exceptions are parsed before any network access follows from the order of the calls in `xtask/src/dependencies.rs`; no test asserts the order.
