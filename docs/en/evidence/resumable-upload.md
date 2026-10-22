# JSON resumable upload final integration evidence

<!-- markdownlint-configure-file {"MD013": false} -->

Verified source: `8d7a25b7eaf8a08e1a6f39b2bb05ac0d618d03e5` on
**2026-10-22**, including the final resumable process-kill tests.
Application source, SDK tests and lockfiles are unchanged from
`1c27c9ebbd6e6f64e538d92917f47ce827ed5c18`, the source of the native SDK
and resource runs. Container/guide source
`05001b1fc8b84c072b1adc163e61453df2b7d04b` refreshes only the Rust builder
digest, dependency snapshot and freshness evidence relative to that application
source. Its Docker/Compose/guide inputs and application source are unchanged in
the verified test tree. The final Rust checks include the new crash tests;
native Go, resource, container and guide runs were not repeated for test-only
changes. Documentation and ledger reconciliation follow the recorded checks.

## Inputs and environment

- macOS 27.0.1 (26A434), Darwin 27.0.0, ARM64, Mac16,11;
  68,719,476,736 bytes RAM, 14 logical CPUs and APFS-backed native storage.
- Rust 1.99.1 (`58cf8f9c35b355199727627d07ae34a4e2846d67`,
  2026-10-13), `aarch64-apple-darwin`, LLVM 23.1.1; Cargo 1.99.1
  (`5f94df478`, 2026-08-27), edition 2024.
- Pinned Go 1.27.3, Storage `cloud.google.com/go/storage` v1.70.0,
  API `google.golang.org/api` v0.307.0, OAuth2 v0.37.0.
  Explicit local endpoints and fixture credentials; cloud credential variables
  cleared, no Google bucket or metadata-server requests.
- Docker client/server 29.8.1, ARM64 daemon, Compose v5.5.1,
  buildx v0.37.1. ARM64 container execution is native; AMD64 uses emulation.
- Unchanged lockfile SHA-256 values: `Cargo.lock`
  `a53256841d1e1f57d8a994237e1f0d903fb1bcb387989ac8b13da58084cb967d`;
  `sdk-tests/go/go.mod`
  `8fd0f4e9aebab325b6941543187e46c025ed9a50f1fab200ace7a0371f2a90e3`;
  `sdk-tests/go/go.sum`
  `e25a221b7d40e4ca4f5ad90c73084b6df714cd4878697cd702fe85a946fd8bb3`.
- Measured native release binary SHA-256:
  `c9b70c3265a1f41ad31acc06820770e226857eec0cfc16150d7f533e789cd83d`.

The [slice contract](../slices/resumable-upload.md) decomposes request fields,
responses, permissions, side effects and error cases. Official sources were
checked on 2026-10-22; fetched-byte digests, Discovery revision `20261013`,
googleapis commit `d73dfb3c3be52245625b951b9c0bcf65163c549f` and SDK source
pins are in [Sources](../sources.md#resumable-upload-source-pins).
The [performing guide](https://docs.cloud.google.com/storage/docs/performing-resumable-uploads)
and [concept guide](https://docs.cloud.google.com/storage/docs/resumable-uploads)
establish initiation, persisted-prefix progress, final visibility, capability
authorization and one-week validity. Underspecified edge behavior remains
explicitly local. fake-gcs-server v1.56.1 and storage-testbench are comparison
references only; neither was executed as a conformance authority.

## Commands and actual results

| Command | Result | Raw output |
| --- | --- | --- |
| `just check` | PASS, exit 0: 343 Rust tests, zero failures/ignored; fmt, clippy, compatibility, shell and release checks | [checks](resumable-upload-checks.txt) |
| `cargo +1.99.1 test --locked --offline --test object_http resumable_sigkill -- --nocapture`, 20 consecutive runs | PASS: 20/20 runs, 100 tests, zero failures/ignored or retried runs | [checks](resumable-upload-checks.txt) |
| `just check-sdk` with cloud credential variables cleared | PASS, exit 0: Go vet, 26 top-level tests and 69 subtests; zero failures/skips | [Go SDK](resumable-upload-sdk-go.txt) |
| `just check-containers` on final builder input | PASS, exit 0: AMD64/emulated and ARM64/native ordinary and seeded Compose scenarios | [containers](resumable-upload-containers.txt) |
| Every executable `sh` block in `docs/en/getting-started.md`, in document order | PASS, exit 0: all 41 blocks, host/peer/browser-address resumable journeys, strict/permissive SDK and persistence/reset | [guide](resumable-upload-guide.txt) |
| `just check-dependencies` at slice start | PASS, exit 0: all 31 inputs CURRENT | [slice-start dependencies](resumable-upload-dependencies.txt) |
| `just check-dependencies` at slice end, plus independent official registry inspection and native ARM64 Docker build | PASS after adopting refreshed Rust builder index; all 31 inputs CURRENT, no dependency exception | [freshness](resumable-upload-freshness.txt) |
| `cargo build --release --locked`; syntax/shellcheck; `bash scripts/measure-resumable.sh` | PASS: one complete native measurement run, five workloads, twelve matching SHA-256 downloads, zero transfer errors | [resources](resumable-upload-resources.txt) |

The container log retains an obsolete-builder attempt intentionally stopped
before Compose startup (exit 130); only the subsequent final-source exit 0 is
passing evidence. The dependency logs retain the initial sandbox lookup failure
and later builder-index drift, followed by successful approved checks.
No heavy command was rerun merely to produce this summary.

## Acceptance case mapping

All names below appear as passing tests in the [checks](resumable-upload-checks.txt)
or [Go SDK](resumable-upload-sdk-go.txt) log unless identified as a guide/resource
observation. HTTP tests are in `tests/resumable_http.rs`, lifecycle tests in
`tests/resumable_lifecycle.rs`, storage tests in `tests/resumable_storage.rs`,
process-kill tests in `tests/object_http.rs`, and Go resumable tests in
`sdk-tests/go/objects_resumable_test.go`.
PASS covers the declared local subset, not unresolved provider wire parity.
All 44 scoped cases have passing evidence; the broader parent cases remain
explicitly pending in the ledger.

| Case | Result | Recorded tests or observation |
| --- | --- | --- |
| RSM-INIT-001 | PASS | `initiation_returns_opaque_fixed_location_and_status_without_publication` |
| RSM-INIT-002 | PASS | `initiation_preserves_encoded_names_metadata_and_query_precedence`; `sequential_binary_chunks_publish_exact_bytes_hashes_and_metadata_only_at_completion` |
| RSM-INIT-003 | PASS | `invalid_initiation_and_status_leave_no_session_or_file`; `sdk_initiation_accepts_one_retry_token_without_generation_conditions` |
| RSM-INIT-004 | PASS | `initiation_checks_strict_create_and_overwrite_permissions_before_staging` |
| RSM-INIT-005 | PASS | `initiation_session_cap_and_status_prefix_are_bounded`; `retained_session_cap_is_atomic_and_rejection_creates_no_file` |
| RSM-CHUNK-001 | PASS | `sequential_binary_chunks_publish_exact_bytes_hashes_and_metadata_only_at_completion` |
| RSM-CHUNK-002 | PASS | `unknown_total_sdk_post_duplicate_overlap_and_empty_final_preserve_prefix` |
| RSM-CHUNK-003 | PASS | `single_empty_and_exact_multiple_uploads_replay_original_completion_after_overwrite` |
| RSM-CHUNK-004 | PASS | `malformed_ranges_and_wrong_lengths_never_advance_the_durable_prefix`; `contradictory_chunk_lengths_fail_before_reading_body_or_acquiring_session` |
| RSM-CHUNK-005 | PASS | `unknown_total_sdk_post_duplicate_overlap_and_empty_final_preserve_prefix` |
| RSM-CHUNK-006 | PASS | `malformed_ranges_and_wrong_lengths_never_advance_the_durable_prefix`; `interrupted_chunk_releases_writer_and_preserves_only_acknowledged_bytes`; `acknowledged_prefix_resumes_through_router_after_restart_and_expired_writes_fail` |
| RSM-CHUNK-007 | PASS | `busy_session_rejects_competing_writer_while_other_sessions_progress`; `chunked_cap_overflow_and_disconnected_transfers_release_all_body_slots`; `session_deadline_during_transfer_returns_not_found_without_acknowledging_tail` |
| RSM-CHUNK-008 | PASS | `unknown_total_sdk_post_duplicate_overlap_and_empty_final_preserve_prefix` |
| RSM-CHUNK-009 | PASS | `object_caps_apply_to_known_unknown_and_streamed_lengths`; `chunked_cap_overflow_and_disconnected_transfers_release_all_body_slots`; `host_follows_returned_location_and_streams_large_body_in_small_buffers`; native large/slow/concurrent resource run |
| RSM-STATUS-001 | PASS | `initiation_returns_opaque_fixed_location_and_status_without_publication`; `single_empty_and_exact_multiple_uploads_replay_original_completion_after_overwrite` |
| RSM-STATUS-002 | PASS | `initiation_session_cap_and_status_prefix_are_bounded`; `sessions_persist_options_offsets_deadline_and_exclusive_admission`; `acknowledged_prefix_resumes_through_router_after_restart_and_expired_writes_fail` |
| RSM-STATUS-003 | PASS | `single_empty_and_exact_multiple_uploads_replay_original_completion_after_overwrite` |
| RSM-STATUS-004 | PASS | `invalid_initiation_and_status_leave_no_session_or_file`; `malformed_ranges_and_wrong_lengths_never_advance_the_durable_prefix` |
| RSM-CANCEL-001 | PASS | `anonymous_cancel_removes_staging_and_terminal_errors_follow_original_deadline`; `strict_anonymous_cancellation_rejects_bodies_and_follows_original_deadline` |
| RSM-CANCEL-002 | PASS | `anonymous_cancel_removes_staging_and_terminal_errors_follow_original_deadline`; `cancel_reloads_terminal_state_when_transition_no_longer_matches`; `strict_anonymous_cancellation_rejects_bodies_and_follows_original_deadline` |
| RSM-CANCEL-003 | PASS | `cancel_obeys_writer_lease_and_only_one_cancellation_wins`; `completed_session_cannot_be_cancelled_or_have_its_bytes_removed`; `cancellation_racing_append_respects_writer_and_prevents_later_publication`; `cancellation_racing_finalization_has_one_terminal_winner_and_preserves_live_bytes` |
| RSM-EXPIRY-001 | PASS | `expiry_is_fixed_at_seven_days_and_checkpoint_never_renews_it`; `session_deadline_during_transfer_returns_not_found_without_acknowledging_tail` |
| RSM-EXPIRY-002 | PASS | `expiry_cleanup_runs_before_reopen_returns_and_releases_retained_slots`; `idle_scheduler_sleeps_and_new_sessions_rearm_due_cleanup`; `expiry_waits_for_writer_and_retries_after_lease_release` |
| RSM-EXPIRY-003 | PASS | `cleanup_failure_is_visible_durable_and_retryable_without_losing_the_slot`; `scheduler_retries_failed_cleanup_and_clears_its_error_after_repair`; `failed_finalization_and_cancel_cleanup_retry_before_session_deadline`; `cleaned_cancelled_sessions_are_not_reclaimed_again_before_their_deadline`; `checksum_error_is_preserved_when_terminal_cleanup_fails_and_is_retried` |
| RSM-FINAL-001 | PASS | `sequential_binary_chunks_publish_exact_bytes_hashes_and_metadata_only_at_completion` |
| RSM-FINAL-002 | PASS | `sequential_binary_chunks_publish_exact_bytes_hashes_and_metadata_only_at_completion` |
| RSM-FINAL-003 | PASS | `checksum_and_final_metadata_failures_preserve_the_previous_full_resource`; `checksum_error_is_preserved_when_terminal_cleanup_fails_and_is_retried`; `TestObjectResumableChecksumFailurePreservesObject` |
| RSM-FINAL-004 | PASS | `all_four_initiation_conditions_are_rechecked_atomically_and_cannot_be_relaxed` |
| RSM-FINAL-005 | PASS | `two_create_if_absent_sessions_have_one_finalization_winner`; `all_four_initiation_conditions_are_rechecked_atomically_and_cannot_be_relaxed`; shared `conditional_overwrite_delete_and_bucket_delete_upload_races_are_atomic` regression |
| RSM-FINAL-006 | PASS | `capability_delegate_uses_initiator_grants_and_rechecks_overwrite_and_revocation`; `TestStrictAuthResumableCreatorOverwriteRace` |
| RSM-FINAL-007 | PASS | `single_empty_and_exact_multiple_uploads_replay_original_completion_after_overwrite`; `retryable_publish_and_commit_failures_preserve_the_prefix_and_allocate_one_generation`; `finalization_catalog_wait_is_bounded_and_publication_may_finish_after_timeout` |
| RSM-FINAL-008 | PASS | `sequential_binary_chunks_publish_exact_bytes_hashes_and_metadata_only_at_completion`; `final_metadata_and_completion_snapshot_survive_catalog_restart` |
| RSM-RECOVERY-001 | PASS | `resumable_sigkill_before_offset_commit_truncates_unacknowledged_tail`; `resumable_sigkill_after_chunk_acknowledgment_preserves_range_and_bytes`; `restart_discards_write_before_offset_commit_and_preserves_committed_prefix`; `acknowledged_prefix_resumes_through_router_after_restart_and_expired_writes_fail` |
| RSM-RECOVERY-002 | PASS | `resumable_sigkill_after_final_checkpoint_restores_active_staging`; `resumable_sigkill_after_final_rename_restores_staging_and_preserves_old_object`; `resumable_sigkill_after_final_commit_preserves_object_and_completion_snapshot`; `restart_restores_rename_before_commit_instead_of_deleting_acknowledged_bytes`; `restart_after_commit_preserves_completed_snapshot_and_live_bytes_past_session_expiry` |
| RSM-RECOVERY-003 | PASS | `missing_short_and_nonfile_acknowledged_prefixes_block_readiness`; `recovery_pages_active_prefixes_and_reclaims_orphans_and_deferred_blobs`; `recovery_rejects_ambiguous_active_publication_without_deleting_either_copy`; `recovery_rejects_symlink_prefix_without_changing_its_target`; `expiry_cleanup_runs_before_reopen_returns_and_releases_retained_slots` |
| RSM-LOC-001 | PASS | `host_follows_returned_location_and_streams_large_body_in_small_buffers`; `TestObjectResumableWriter`; guide host follows returned Location unchanged |
| RSM-LOC-002 | PASS | Guide Compose peer follows `http://gcs:4443` Location and verifies 9 bytes; host curl follows the configured localhost browser-address Location, then cancels with 499. No browser/CORS claim |
| RSM-LOC-003 | PASS | `initiation_returns_opaque_fixed_location_and_status_without_publication`; `config::tests::advertised_endpoint_precedence_and_validation`; `url::tests::normalizes_base_urls`; `url::tests::rejects_invalid_base_urls` |
| RSM-SDK-GO-001 | PASS | `TestObjectResumableWriter/default_chunk_size_resumable`: object exceeds default 16 MiB threshold; actual resumable transport asserted |
| RSM-SDK-GO-002 | PASS | `TestObjectResumableWriter` including multiple 256 KiB chunks, empty final chunk, empty object, unknown-length stream and small multipart fallback; `TestObjectResumableChecksumFailurePreservesObject` |
| RSM-SDK-GO-003 | PASS | `TestObjectResumableRetriesLostChunkResponse` |
| RSM-SDK-GO-004 | PASS | `TestObjectResumableContextCancellationDoesNotPublish`; `chunked_cap_overflow_and_disconnected_transfers_release_all_body_slots` |
| RSM-SDK-GO-005 | PASS | `TestObjectResumableConditionsAtFinalization`; `TestObjectResumableMetagenerationAtFinalization`; `TestStrictAuthResumableWriter`; `TestStrictAuthResumableCreatorOverwriteRace` |
| RSM-INSERT-001 | PASS | Resumable HTTP/lifecycle/storage suites and existing media/multipart/metadata/authorization/catalog regressions in `just check`; resumable Go tests in `just check-sdk`. Full insert fields, permissions and replay remain pending |

Final review regressions all PASS in the checks log:

| Test | Verified observation |
| --- | --- |
| `retryable_publish_and_commit_failures_preserve_the_prefix_and_allocate_one_generation` | Failure before/after rename keeps the durable prefix; eventual completion allocates one generation |
| `contradictory_chunk_lengths_fail_before_reading_body_or_acquiring_session` | Contradictory lengths fail before body/lease admission |
| `final_hash_scan_uses_request_budget_instead_of_catalog_wait_budget` | Whole-object hashing uses the request budget rather than the shorter catalog budget |
| `finalization_catalog_wait_is_bounded_and_publication_may_finish_after_timeout` | Catalog response wait is bounded; eventual publication is visible through status without a second generation |
| `cleaned_cancelled_sessions_are_not_reclaimed_again_before_their_deadline` | Successful terminal cleanup is not repeated before tombstone expiry |
| `checksum_error_is_preserved_when_terminal_cleanup_fails_and_is_retried` | Cleanup failure preserves the original checksum error and journaled retry |

The coordinator also reported 50/50 passes after fixing
`failed_finalization_and_cancel_cleanup_retry_before_session_deadline`;
the final checks log independently records one passing invocation, not those
50 repetitions. No new repetition count is inferred from that single run.

The guide's permissive Go run passed 15 top-level tests and 47 subtests, with
11 strict-only tests skipped because no strict endpoint was set. Its later
strict run passed all 11 selected top-level tests and 22 subtests without skips.
Only the isolated native SDK run is counted as a full 26-test, no-skip run.

Documentation reconciliation separately passed `cargo xtask compatibility
validate`, `render` and `check`, `git diff --check`, and the required local-path
grep with no matches. markdownlint-cli2 v0.24.0 passed all six changed Markdown
files, including both generated views, with MD013 disabled for existing long
tables/paragraphs and MD024 `siblings_only` for repeated category headings under
different feature kinds. The default lint reports those existing generated-view
layout conventions; no generator or completion policy was changed to silence it.

## Bounded resources and measured results

Source/configuration bounds are 128 retained sessions, eight admitted body
transfers, two copied 64 KiB channel frames per transfer, a 64 KiB hash-read
buffer, 32 catalog admissions and one executing SQLite job. Session and blob
IDs are opaque, object names never form disk paths, and transfers hold no SQL
transaction. Metadata lives in the indexed catalog. These bounds are separate
from the measured RSS values below.

The native release measurement used a fresh permissive server/catalog for each
workload, loopback HTTP/1 without TLS, FULL synchronous WAL, a 2 MiB SQLite cache
and an explicit 2 GiB object cap. Each logical object repeats an 8 MiB random
disk fixture; curl streams it. Eight concurrent 256 MiB sessions total 2 GiB.
The slow case limits each PUT to 1 MiB/s. Expected and downloaded SHA-256 values
match for all twelve objects; no transfer errors occurred.

| Workload | Object MiB × sessions | Sampled upload peak RSS MiB | Lifetime maximum RSS MiB | Upload CPU seconds | Upload wall seconds | MiB/s | Final request seconds |
| --- | --- | ---: | ---: | ---: | ---: | ---: | --- |
| Small | 16 × 1 | 6.80 | 7.16 | 0.06 | 0.101039 | 158.354 | 0.053073 |
| Medium | 256 × 1 | 7.20 | 7.73 | 0.98 | 1.329243 | 192.591 | 0.441350 |
| Large | 1024 × 1 | 7.23 | 7.81 | 3.66 | 5.089183 | 201.211 | 1.676408 |
| Slow | 16 × 1 | 6.95 | 7.25 | 0.14 | 16.161522 | 0.990 | 8.098895 |
| Concurrent | 256 × 8 | 10.33 | 10.61 | 6.01 | 4.101328 | 499.350 | 1.504904–1.542089 |

| Workload | CPU seconds/GiB | Chunk requests | Chunk latency p50 seconds | p95 seconds | Maximum seconds | Post-upload disk KiB |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Small | 3.840 | 2 | 0.017441 | 0.017441 | 0.053073 | 16568 |
| Medium | 3.920 | 32 | 0.017044 | 0.020504 | 0.441350 | 262488 |
| Large | 3.660 | 128 | 0.015176 | 0.019770 | 1.676408 | 1049316 |
| Slow | 8.960 | 2 | 8.020220 | 8.020220 | 8.098895 | 16664 |
| Concurrent | 3.005 | 256 | 0.064201 | 0.087254 | 1.542089 | 2166888 |

RSS samples use `ps` every 0.05 seconds plus sampling overhead; they can miss
short transients. Lifetime maximum RSS uses `/usr/bin/time -l` and includes
startup, uploads, verification downloads and shutdown. Upload CPU uses `ps`
time deltas at 0.01-second precision and excludes clients/downloads. Upload
wall time excludes initiation and verification. Latencies are per-request
`curl time_total`; final requests include the last chunk and whole-object hash
scan. Quantiles are lower empirical order statistics from one run, not repeated
trials. Disk is post-upload `du -sk`, including catalog/WAL/SHM, not a sampled
high-water mark. p99 was not recorded. Linux cgroup memory, page-cache usage,
memory pressure, and a 1 GiB transfer under a 128 MiB container limit were not
measured. No equivalent-workload comparison or performance improvement is claimed.

## Docker digests

| Input | Final pinned index digest |
| --- | --- |
| `rust:1.99.1-slim-trixie` | `sha256:aab42391a986424e22d6a3ec14f37bc6dd3dacff4d457ca7cccd781e02297cc6` |
| `gcr.io/distroless/cc-debian13:nonroot` | `sha256:83b9b50586253821cf63ffcec87d3d94265a093ad30da232039af962dd8f4edf` |
| `docker/dockerfile:1` | `sha256:4edf897a3ffa55b89f906fc8cc78afdb3f1834cc9c7083565e611a8a7d5fe99e` |
| Peer `curlimages/curl:8.23.0` | `sha256:11e64e28c5f58c8d93ee95fcf22ac055f48b0bc440e0ce7d8013bbb5ad6aa8ac` |

The Rust platform manifests did not change: AMD64
`sha256:d7d8fd94f01c80a355b86fe8691abc26be051daa44fa021a8b5ece5c8cbcb469`,
ARM64/v8 `sha256:73b81f338853a11a828adeb6fc31ba5d53e3cbc435fb6ae71752473242f6f88b`.
Actual BuildKit outputs, including their distinct attestation-bearing indexes:

| Build | Image manifest | Local index |
| --- | --- | --- |
| Container smoke AMD64 | `sha256:157ef8afdbbb568e20e5fa6f45e8760966397b8b3bd88d04c474eebec6847af8` | `sha256:9328be060880015cd0446e05816c6acd9722c5bffcae54928ceef0e7b4811428` |
| Container smoke ARM64 | `sha256:c52a51adca73289415556f4f37940935e84d77909bceb002db7c3b637c1c843d` | `sha256:bb5e4469c2fb13a1cadee16df945e0e50be57c3c308c3d88c8c1438079c47abc` |
| Guide ordinary Compose ARM64 | `sha256:c52a51adca73289415556f4f37940935e84d77909bceb002db7c3b637c1c843d` | `sha256:965d9e739294911d73cff41aaf4870cb8d3aaf1d591ad609d4d4051d869f6177` |
| Guide seeded Compose ARM64 | `sha256:c52a51adca73289415556f4f37940935e84d77909bceb002db7c3b637c1c843d` | `sha256:374226b6b90e2974390317a4c9eb970b73ed978cbb7fdb387e959a956272c572` |

## Ledger judgement and limits

| Feature | Status / achieved fidelity | Evidence boundary |
| --- | --- | --- |
| BEHAVIOR-OBJECT-010 | IMPLEMENTED / EXACT | All five initiation cases pass within the frozen JSON/local-auth scope |
| BEHAVIOR-OBJECT-011 | IMPLEMENTED / EXACT | All nine chunk cases pass, including measured large/slow/concurrent bounded buffering |
| BEHAVIOR-OBJECT-012 | IMPLEMENTED / EXACT | All four status cases pass, including persistent prefix and completed resource snapshot |
| BEHAVIOR-OBJECT-013 | IMPLEMENTED / EXACT | All six cancellation/expiry cases pass under the declared injected-clock local session model |
| BEHAVIOR-OBJECT-014 | IMPLEMENTED / EXACT | All eleven finalization/recovery cases pass, including five native process-kill boundaries and 20 repeated crash runs |
| DESIGN-057 | IMPLEMENTED / PROJECT | All three fixed-advertised-endpoint cases pass; browser-address curl is not browser/CORS execution |
| JSON-OBJECTS-007 | PARTIAL / LOCAL_SIMULATION | Scoped resumable contribution passes; broader method fields, IAM/ACL, encryption, Discovery alias and response replay remain incomplete |
| DESIGN-082 | PARTIAL / PROJECT | All five resumable Go cases pass; signing, broader IAM/ACL, XML breadth, gRPC and token replay remain incomplete |

No existing acceptance case is narrowed. Actual native process kills cover file
write before offset commit, an acknowledged chunk, the final durable checkpoint
before rename, rename before Object/COMPLETE commit, and successful final commit.
Prepublication barriers use a fixed 64 MiB disk prefix, bounded 1 ms polling and
a five-second boundary deadline; missing the window fails the test. These are
session-specific checks in addition to ordinary media SIGKILL regressions.
SQLite and rename are separate: sync/offset checkpoints preserve only committed
bytes; precommit rename is restored to active staging, and Object plus COMPLETE
snapshot commit together. After the final checkpoint/rename crash, the recovered
Range can cover the full durable prefix despite a lost final HTTP response.
Injected-state reopen tests additionally fail closed on damaged/ambiguous
prefixes. Neither set establishes every instruction/fsync window or power loss.

Known limits retained from final review:

- Finalization rereads and hashes the whole object with bounded buffers; latency
  is proportional to object size and uses the absolute request deadline.
- When failure occurs before publication starts, resolution SQL in
  `resolve_resumable_finalization` uses the remaining request deadline
  (default 900 seconds), rather than the catalog-wait budget (30 seconds).
- Publication may complete after a response timeout; a held session lease
  prevents a second publication. Query status before assuming failure.
- The checksum-cleanup regression test retains a theoretical DELETE-410 versus
  scheduler-lease race that can transiently return 503. The hash-budget test's
  ability to distinguish the old behavior depends on machine speed.
- Resumable publication has its own helper rather than sharing ordinary insert
  publication. The nonfinal timeout diagnostic also says finalization timed out.

SPEC-RSM-DISCOVERY-PATH, SPEC-RSM-EDGES, SPEC-RSM-AUTH-CONDITIONS and
SPEC-RSM-LOCATION remain OPEN in [the gap ledger](../../../compatibility/gaps.json).
They cover the Discovery alias, local edge/error/terminal-retention choices,
provider permission-check timing/order, and deferred automatic allowed-origin
selection. SPEC-013 remains OPEN: initiation accepts one nonempty retry token
without `ifGenerationMatch`, creates a distinct bounded session on retry, and
does not replay the response. Media/multipart keep their generation-match
requirement. Session offsets/completion snapshots are not invocation-token replay.
SPEC-008/010/011/012/015/016/017 retain applicable parent-method questions.

XML resumable/multipart, signed initiation (DESIGN-042), gRPC, other SDKs,
browser CORS and full ACL/UBLA/PAP remain deferred. No real-cloud differential
tests, native AMD64 execution, remote CI, publication, Linux/cgroup resource
measurement or power-loss verification were performed. Containers/volumes/
networks and the exact guide/smoke image tags were cleaned up in the logs.
The resumable deliverable moves to existing scoped evidence; the next
implementation capability is V4 RSA signed download (order 5).
Raw logs redact local path prefixes and, in the guide, session capability values.
