# Typed administrative storage boundary

<!-- markdownlint-configure-file {"MD013": false} -->

Scope: DESIGN-062, DESIGN-072 and DESIGN-074, with UX-002 and existing AUDIT-014 regression coverage. Reviewed implementation: `dbfe9de56f1858a785702d6eea968b40fb77cb00`, rebased onto integration source `cead819e2a96fc2dd72188953a385aadac276cfd`. The final verification and measurement source is `f74a55709d74cfe239297117c451c29cec0fb70c`; its runtime/test/build/harness inputs are identical to the reviewed implementation. The schema remains 14; dependencies, compiler pin and lockfile are unchanged. No external workflow, push, package publication or Google Cloud operation was performed.

## Native acceptance

The following tests pass in `src/storage/admin_tests.rs` through the public administrative operations or their shared bounded-admission primitive. Test names are unchanged by module qualification.

| Test | Verified behavior |
| --- | --- |
| `admin_names_generations_and_permissions_are_validated` | Native bucket/object validation, positive nonzero generations and closed permission enumeration. |
| `admin_ensure_batches_preserve_identity_order_and_rollback` | Created/Existing in input order, actual existing owner/settings, empty/duplicate/oversized batches, rollback after a later SQL rejection, bounded bucket pages and restart. |
| `admin_ensure_full_batch_and_concurrent_creates_are_atomic` | Inclusive 1000-record batch and competing transactions create each bucket once. |
| `admin_admission_is_independent_of_saturated_api_slots` | All 32 API call slots and eight API upload slots occupied; management waits for execution, publishes, reads and pages successfully. |
| `admin_wait_deadlines_queue_limit_and_closed_state_are_typed` | Both slot/execution wait paths return Busy, the 32-waiter bound rejects overflow, and close returns Closed. |
| `admin_publication_times_out_at_the_public_admission_deadline` | Public publication actually waits 30 seconds under occupied management slots, then returns Busy without publishing. |
| `admin_cancelled_ensure_drains_the_owned_sql_job_before_unlock` | Cancellation while SQLite waits on a real write lock cannot release directory ownership; the accepted create completes before close/reopen. |
| `admin_replace_preserves_conditions_checksums_metadata_and_versions` | Supplied custom metadata/MIME, checksum rejection, one-winner generation condition, monotonic generations, retained old bytes and version-aware deletion; default policy does not grant strict anonymous API reads. |
| `admin_holds_and_disk_full_keep_their_error_types` | Default hold blocks replacement/deletion; streamed StorageFull and actual SQLite SQLITE_FULL retain DiskFull, roll back the full bucket batch and preserve the existing object. |
| `admin_cancelled_body_cleans_staging_before_unlock` | Cancelled streaming publication removes staging before close and creates no readable object. |
| `admin_shutdown_holds_lock_through_renamed_publication` | Cancellation after immutable rename cannot unlock before commit/cleanup; reopening reads committed bytes and staging is empty. |
| `admin_invalid_metadata_cannot_replace_a_readable_generation` | Shared fixed-metadata validation rejects CR/LF, controls, overlong language and empty custom keys as Invalid before replacing the original generation/bytes. |
| `admin_corrupt_sqlite_and_stored_values_remain_typed` | SQLITE_CORRUPT, SQLITE_NOTADB and malformed stored bucket/object/policy values retain Corrupt through management reads without string parsing. |
| `disk_full_during_resumable_retirement_preserves_the_same_session_and_bytes` | A real SQLite page limit and retirement trigger force SQLITE_FULL after durable offset/rename; the same active session retains all received bytes and succeeds on retry. |
| `disk_full_during_derived_retirement_preserves_the_same_transfer_and_bytes` | The same retirement failure preserves derived transfer/checkpoint bytes and the old destination; retry publishes from the retained transfer. |

The native command `cargo test --workspace --locked --offline` passed **796 tests across 33 reported suites**, with zero failures and zero ignored tests, including 338 library tests, seven binary tests, the 13 native seed tests and 19 foundation tests. `cargo clippy --workspace --all-targets --locked --offline -- -D warnings` and `cargo fmt --all` passed. Localhost tests ran outside the restricted sandbox because bind was denied there. This environment setup does not change the source or relax any assertion.

The old catalog, object-storage and resumable storage/lifecycle/HTTP suites now reside in `src/storage/` with `_tests.rs` names. Every function name and its order is preserved; exact child-process filters include their new module qualification. They execute with `cargo test --lib storage::` and the full workspace command. The raw SQL callbacks, session construction/lease/staging paths and internal transfer types are crate-private. Compile-fail documentation also checks grant construction and the external handle/callback boundary. This closes public raw access without creating a test-only public bypass. Shared downgrade fixtures remain identical. Historical evidence retains the commands and source paths actually used at that time.

## Final verification and measurements

Final verification on 2026-10-24 used the reviewed source in the coordinator's resource window. [Raw command output](shared-admin-interface-checks.txt) retains successful checks and the environment failures described below; no assertion was weakened.

| Command/check | Result |
| --- | --- |
| `just check` | PASS: formatting, Clippy, 796 tests in 33 suites, ledger and offline script checks. |
| `just check-sdk`, Go portion | PASS: pinned Go 1.27.3, storage 1.70.0 and API 0.308.0; vet and all tests. The combined recipe stopped at Ruby's version preflight because the initial default was 3.4.1. |
| Ruby SDK with pinned Ruby 4.0.9, Bundler 4.0.23, google-cloud-storage 1.62.1 | PASS separately: emulator-origin self-check and permissive/strict V4 GET/PUT, response overrides, bucket/object methods, derived bytes and resumable chunks. Only already locked gems were installed into an isolated temporary bundle; manifests/lockfiles are unchanged. |
| Linux ARM64, network disabled, Rust 1.99.1: `cargo +1.99.1 test --locked --offline --test seed` | PASS: all 14 tests. An initial run lacked the read-only `config/identities.json` fixture and failed only the strict-auth case; adding the configuration mount passed that case and the complete suite. |
| `bash scripts/smoke-containers.sh linux/arm64 linux/amd64` | PASS for both architectures: probes/CRUD/persistence/resource limits/signals and read-only seed lifecycle. AMD64 is emulated. |
| Release binary and measurement example build | PASS with `--locked --offline`. |
| Measurement example tests and shell self-check | PASS: three example tests; unknown revisions, tracked/untracked runtime changes, interruption/timeout cleanup, directional CPU intervals and five-run calibration. |

The Linux test builder reported the pre-existing unused `time::Duration` import in `src/startup.rs`; native Clippy remains clean. Expected negative TLS/healthcheck diagnostics are present in successful regression output. Neither diagnostic was suppressed.

The native measurement completed five repetitions each of small-publish, small-overwrite, upload/download of 1 GiB, startup-10000 and startup-100000 under the existing shared contract: **30 samples, zero errors, 39 calibrated metric entries PASS, zero FAIL and one inherited CPU-calibration PENDING**. [Raw preparation/measurement/resource output](shared-admin-interface-benchmark.txt), [all candidate samples and medians](shared-admin-interface-candidate.json) and [every frozen-bound comparison](shared-admin-interface-comparison.json) preserve full precision and every outlier. Sync/import workloads remain unavailable.

| Workload | Wall median / frozen upper (s) | Server CPU median / frozen upper (s) | RSS median / frozen upper (KiB) |
| --- | --- | --- | --- |
| small-publish | 116.897154 / 150.593643 | 18.08 / 36.06 | 11600 / 11915.2 |
| small-overwrite | 169.104483 / 209.272240 | 24.89 / 41.76 | 11536 / 11915.2 |
| upload-1gib | 3.012373 / 3.171217 | 2.57 / 2.83 | 10288 / 12144 |
| download-1gib | 0.291246 / 0.539930 | 0.53 / 0.76 | 11040 / 12480 |
| startup-10000 | 0.175554 / 0.654544 | 0.08 / PENDING | 10928 / 11140.8 |
| startup-100000 | 1.200823 / 3.561013 | 0.78 / 1.69 | 11024 / 11158.4 |

Display values are rounded; the comparison JSON is authoritative. RSS is the median of sampled phase maxima for HTTP workloads and of readiness observations for startup. Each 1 GiB direction has only one request per repetition, so its p50/p95/p99 are that request's duration, not a population estimate. All latency, allocated phase-end disk and normalized CPU gates also pass in the JSON. Startup-10000 raw CPU values are 0.10, 0.10, 0.08, 0.08 and 0.08 seconds; the existing precision boundary prevents a calibrated CPU comparison. This is not a speed-improvement claim.

The measurement used macOS 27.0.1 (26A434), Apple M4 Pro, 64 GiB RAM, 14 logical CPUs, APFS, Rust/Cargo 1.99.1 and the native permissive WAL FULL profile, with no server container limit. Cache was not evicted. Other workers did not build/test in the reserved window. Four pre-existing user containers remained running: anonymous before/after Docker CPU/memory observations and host load averages are retained in raw output. Their CPU observations ranged from 0.00–2.04% before and 0.00–5.18% after; snapshots cannot exclude intervening background activity. These observations are not server cgroup measurements. Full lifetime RSS peaks, temporary-disk high-water, cold cache and cgroup memory remain unmeasured.

Fixture preparation uses the current `Admin::publish_object` Replace path, ordinary checksums/fsync/WAL FULL and at most eight producer tasks admitted through the two management upload slots. It never inserts directly into SQLite. Preparation is outside every measured interval and verifies the 10,000/100,000/empty fixture counts. Its stdout retains the historical `"publication":"Catalog::put_object"` label; the implementation and recorded harness manifest identify the actual administrative call.

The commands executed for the native measurement are below. A new disposable directory is required; the runner rejects a second measurement of the same prepared directory. After integration, an equivalent source pin can be selected only after the source/test identities and input manifests below match; preserve that selected pin in the new raw output.

```sh
bench_source=f74a55709d74cfe239297117c451c29cec0fb70c
cargo +1.99.1 build --release --locked --offline --bin gcs-mimic --example onboarding-state-benchmark
cargo +1.99.1 test --locked --offline --example onboarding-state-benchmark
bash scripts/test-onboarding-state-benchmark.sh
bench_work=$(mktemp -d)
bash scripts/measure-onboarding-state.sh prepare "$bench_work" "$bench_source"
# Reserve the coordinator's exclusive resource window before measurement.
bash scripts/measure-onboarding-state.sh measure "$bench_work" "$bench_source" > benchmark.txt
jq -Rs -f scripts/summarize-onboarding-state.jq benchmark.txt |
  jq --arg source "$bench_source" '.source = $source' > candidate.json
```

The final jq step explicitly replaces the historical summarizer's hard-coded baseline source; it does not alter any sample or numeric field. The canonical baseline and its original raw output remain unchanged.

The frozen `60a4f03` baseline uses rusqlite 0.40.2/libsqlite3-sys 0.38.2. The explicitly assigned integration base and this candidate use 0.41.0/0.39.0, with earlier serde/libc/dev-proptest additions also changing the build graph. The compiler, hardware, runtime profile, workload and correctness assertions match; each source has its locked graph and input digests recorded. Under the shared contract, the candidate's five-run medians are compared against the historical frozen bounds and every numeric result is retained. The graph/runtime differences prevent attributing a result solely to administrative overhead. This slice changes no dependency; no separate causal-control experiment is claimed.

The slice-start official dependency detector found Rust/direct crates/Go/Ruby current and only the already known distroless digest update. That update is outside this slice and remains a visible release gate.

Final documentation checks pass: `cargo xtask compatibility validate`, `render` and `check` validate all 591 feature rows and leave the generated English/Japanese matrices unchanged. `npx --yes markdownlint-cli2` reports zero issues in the eight changed Markdown documents. The durable raw benchmark regenerates the committed candidate JSON byte-for-byte after the explicit source replacement; the comparison asserts all 40 entries and the 39/0/1 PASS/FAIL/PENDING counts. Ledger changes are confined to DESIGN-062/072/074, UX-002 and existing AUDIT-014 regression evidence; broad feature statuses remain unchanged.

The raw Docker/Compose output in `shared-admin-interface-checks.txt` retains its original trailing spaces and final blank line, as required for unmodified raw evidence. Git's whitespace check consequently reports those raw-output lines; all other staged files pass the same check. Local-path prefixes alone are redacted, with a notice at the start of the log. The path scan finds only the pre-existing planning instruction containing its own search pattern; no local path or session task/review identifier was added.

## Source provenance

These Git object identities and relative-path manifests remain verifiable after integration squashes intermediate commits. `git ls-tree -d HEAD src tests` must retain `src` tree `861addb9edec1f6606b861d2a08d561e4e04754b` and `tests` tree `e785caaf6389199e899d3860bf9ec67eae57481e`; other source changes require new verification. The build manifest also covers the embedded Discovery schema and signing fixtures outside those trees. The SDK/container manifest records selected launch/configuration and dependency inputs; it is supplementary to the source/test identities, not a complete image digest.

| Input manifest | SHA-256 |
| --- | --- |
| [Build inputs](shared-admin-build-inputs.txt) | `447c1a4a269bbe1bb47400e5d871909187e0ca6af4a00126c54ab166a60e4c71` |
| [Measurement example and harness](shared-admin-harness-inputs.txt) | `387f4ca8ac3d12df32deaef0c2acc1e0c1f5881e92ed4a30cbf6c9b92dd855d2` |
| [Selected SDK/container inputs](shared-admin-sdk-container-inputs.txt) | `6d519e764e958e890adb60cd38e4a508a5311b6bc289223b91b75185d79115d7` |

Recreate a manifest with `git ls-tree -r HEAD` followed by exactly its listed paths, preserving Git's sorted output, then compare its SHA-256. Compiler, lockfile and measured release-binary SHA-256 values are additionally printed by the measurement runner. Source, tests, manifests and measurement inputs were unchanged between the reviewed implementation, final checks, fixture preparation and measurement.

## Limits

Origin is carried by AdminGrant; no durable change journal exists yet. Replace reuses the current media publisher, fsync/recovery protocol and cancellation ownership. InheritFromReplaced, SkipIfSameHash, staging handles, file/batch publication, transactional adapter state, journals and later migrations remain unimplemented. The initializer's startup wiring and its end-to-end create-missing/kill/resource cases remain its own slice. Schema 14 regression does not establish complete backup restoration, downgrade, host power loss or every crash instruction boundary. Windows/native AMD64 and cgroup measurements are not established by these native tests.

The two management upload slots, 32 read slots, 32 simultaneous admission waiters and 30-second admission deadline are fixed local bounds. The single SQL/filesystem job and an accepted body transfer have no new execution wall-clock limit; cancellation/close drains accepted side effects rather than aborting commit or deleting possibly committed data. Seed's existing stable, operator-owned read-only source requirement and traversal limits remain unchanged.
