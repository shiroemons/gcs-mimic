# Ready event directory contract verification

<!-- markdownlint-configure-file {"MD013": false} -->

Verified on 2026-11-11 with Rust 1.99.1 on macOS ARM64. Scope: DESIGN-067; the parent row remains PARTIAL. The [startup contract](../slices/startup-phases.md) defines the fields and the [release notes](../releases/0.18.0-contract-changes.md#ready-event-fields) describe the required script edits.

## Process acceptance

Every case starts the actual executable with parsed CLI/environment inputs and checks stdout ready JSON. Creation checks also read stderr JSON and require exactly one info-level `data_dir_created` record naming the same absolute path; reopening requires none.

| Test | Verified behavior |
| --- | --- |
| `ready_event_reports_absolute_data_directory_and_restart_state` | An absolute missing directory reports `created`, then `opened` on restart. |
| `ready_event_resolves_relative_and_default_data_directories_from_working_directory` | Explicit relative paths and the unchanged default `data` resolve from the child's working directory; restart reports `opened`. |
| `ready_event_calls_an_existing_directory_without_a_catalog_created` | An existing directory without a database and an empty database both report `created` on initialization, then `opened`. |
| `ready_event_preserves_non_ascii_data_directory` | Non-ASCII path text survives ready and the creation diagnostic. |
| `ready_event_preserves_the_symbolic_link_spelling_of_the_data_directory` | A relative symlink becomes absolute without replacing the symlink name with its target. |
| `ready_event_omits_data_directory_and_creation_diagnostic_for_memory` | Memory mode omits both directory fields and emits no creation diagnostic; the configured path remains absent. |
| `ready_event_reports_opened_after_a_crash_before_the_first_ready_event` | A full diagnostic socket blocks startup before ready; after schema initialization commits, SIGKILL leaves stdout empty and the next process reports `opened`. |
| `process_shutdown_reports_stopped_and_releases_catalog_for_restart` | The complete disk ready field set excludes `protocol` and `gcs_api`, includes both directory fields and preserves the remaining contract. |

The shared directory assertion also checks that both removed fields are absent. The memory case separately checks their absence.

Existing stderr readers now parse every JSONL record strictly and select `event: "diagnostics"` without changing diagnostic count/key/exit-status assertions. The initializer's 64 KiB budget is measured on the original diagnostic line, including its newline. The interrupted-seed process test pins `data_dir_created` before `seeding`; the media-read failure test still checks every event other than the intentional startup path notice for internal-path disclosure, and retains its warning/cause checks. The two whole-stderr JSON reads in `public_origin_cli.rs` apply to offline URL-helper configuration errors, which never open a catalog, so they are unaffected.

The Linux-only process regression `ready_event_displays_a_non_utf8_working_directory_without_failing_startup` creates a working directory containing an invalid UTF-8 byte and starts with the default relative `data`. It checks successful first startup and restart, complete ready and creation events, and lossy path display while the actual store keeps its original bytes. It is not executed on macOS, whose filesystems reject such names; Linux execution remains for the final platform verification. No claim of Linux execution is made here.

## Commands and results

```text
cargo check
PASS
cargo check --tests
PASS
cargo test --locked --test it_process ready_event_ -- --test-threads=1
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 82 filtered out
cargo test --locked --test it_process process_shutdown_reports_stopped_and_releases_catalog_for_restart
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 88 filtered out
cargo xtask lint
lint: 0 new production violations; 0 ratcheted findings; 1 reasoned suppressions (text::stringified_error: 1); test wall-clock calls within xtask/test-clock-baseline.txt
cargo test --locked
test result: FAILED. 2041 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 390.30s
cargo test --locked --lib server::batch::tests::a_part_past_the_request_limit_answers_alone_and_later_parts_still_run -- --exact --test-threads=1
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 2041 filtered out; finished in 0.85s
```

The complete test attempt stopped after its library phase because the existing 100ms batch deadline test failed at `assertion failed: next_text(&mut body).await.contains("204")` under parallel load. Its standalone rerun passed. No test or deadline was weakened, skipped or removed.

All remaining binary/integration targets were then run with:

```sh
cargo test --locked --no-fail-fast --bin gcs-mimic \
  --test it_admin --test it_import_seed --test it_json --test it_objects \
  --test it_process --test it_protocol --test it_sync --test it_xml_auth \
  -- --test-threads=4
```

That run exposed the stderr-reader assumptions described above: eight initializer cases rejected the new JSONL stream with `Error("trailing characters", line: 2, column: 1)`, the seed interruption case received `data_dir_created` before its expected `seeding`, and the media-read test treated the intentional startup path notice as a request-error disclosure. After correcting those readers and keeping their diagnostic/order/privacy assertions, both affected suites were rerun in full:

```text
cargo test --locked --test it_import_seed --test it_protocol -- --test-threads=4
test result: ok. 94 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 22.18s
test result: ok. 155 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 21.98s
cargo test --locked --doc
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

The other targets passed in the original remaining-target run: binary 32, admin 75 with one pre-existing ignored test, JSON 135, objects 157, process 89, sync 31, XML/auth 317. Every root-package test target was exercised; the one library timing failure passed its standalone rerun. Final production lint remained at zero new violations. Compatibility validate/render/check passed, both generated matrices were unchanged, and all eight changed Markdown files passed markdownlint.

Process tests require permission to bind local sockets. Dependency freshness was checked through the existing official-registry checker: Rust 1.99.1 is current, but `rusqlite: UPDATE_AVAILABLE current=0.41.1 latest=0.42.0` makes `cargo xtask dependencies check` exit 1. Dependency migration is outside this change.

No container build, SDK harness, guide walkthrough, real-cloud call or physical power-loss test was run. Catalog initialization, migrations and durable write steps are unchanged; state comes from the existing version inspection under the directory lock, and path formatting adds no directory scan or request-path I/O. Historical ready-event captures remain unchanged as records of older releases.
