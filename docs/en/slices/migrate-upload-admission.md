# Upload admission during migration

Feature IDs: DESIGN-071, DESIGN-070. This is an emulator resource contract,
not a Google Cloud rate-limit simulation. No new configuration is introduced.

## Acceptance cases

- With `max_connections=32`, 12 and 40 simultaneous uploads all succeed.
- With `max_connections=64`, 64 simultaneous uploads do not fail admission.
- Resumable initiation followed by one final 128 KiB PUT succeeds for 40 uploads
  with 32 connections and for 64 uploads with 64 connections.
- GET and object listing succeed while 40 uploads are active, without a latency
  gate.
- Shutdown waits for active uploads within `shutdown_timeout_ms`; staging files
  and directory ownership are released after completion or cancellation.
- A client disconnect before EOF leaves no staged file or partial publication.
- JSON 503 errors explain known safe causes and recovery steps without exposing
  secrets or filesystem paths.

## Design

The HTTP connection limit is the only API upload concurrency limit. Upload
ownership is tracked until publication and cleanup finish, including detached
cleanup after cancellation. Shutdown atomically stops new upload admission and
waits for the active count to become zero through notification, without polling.
The existing 64 KiB chunks and depth-two body channel remain unchanged.
Administrative upload limits remain separate.

The 64-upload test exposed immediate catalog admission rejection. Upload
prechecks and resumable session creation/read/checkpoint now await the existing
32-operation semaphore; unrelated catalog operations retain immediate rejection.
Execution still uses the single SQLite
slot and short blocking jobs. Concurrent GET/list tests pass, so the blocking
pool configuration is unchanged.
Finalization already waits for the execution slot and retains upload ownership.
Resumable initiation, session acquisition and status checkpoint HTTP waits now
use the same catalog timeout and remaining absolute request deadline as media.
Maintenance can hold catalog admission independently of API connections, so
32 connections alone do not prevent saturation of a 32-operation catalog gate.
Maintenance keeps its existing bounded retries; no new background pool is added.
Existing absolute request and catalog wait deadlines remain applicable.

## Verification

On 2026-10-25, `cargo test --locked --offline --test object_http` passed all
30 tests, including `simultaneous_uploads_follow_the_connection_limit_and_keep_reads_working`,
`simultaneous_resumable_initiation_and_final_chunks_wait_for_catalog_admission`,
`disconnect_cleans_staging_and_shutdown_waits_for_upload_completion`, and
`obj_http_catalog_wait_deadlines_return_backend_error_and_commit_can_finish`.
Targeted library tests also passed:
`stalled_uploads_leave_blocking_workers_available_without_an_upload_limit`,
`shutdown_rejects_new_uploads_and_waits_for_all_active_guards`, and
`backend_causes_are_actionable_and_private_details_stay_private`.

`cargo xtask dependencies check` passed: Rust 1.99.1, all direct Rust libraries,
the pinned SDK/toolchain versions and container inputs are current. No dependency
or lockfile changes were needed. `just check` passed on the final implementation:
formatting, Clippy with warnings denied, the complete workspace test suite,
compatibility validation/generated-view check, and shell/release-input checks.
The library suite passed all 454 cases; object HTTP passed 30 cases, including
the new resumable concurrency tests. No container image was built.

`just check-sdk` passed on the final implementation with Go Storage v1.70.0
and the unchanged Ruby Gemfile.lock: Ruby 4.0.9, google-cloud-storage 1.62.1,
origin self-check,
permissive HTTP operations/resumable chunks, strict operations, and V4 signed
GET/PUT plus response overrides. No SDK source files were changed.

Current tests replace former eight-upload admission rejection with successful
additional uploads and retain interrupted-body rollback and cleanup checks.
Fixed safe 503 causes also include retained-session capacity, busy sessions,
upload finalization timeout and copy request timeout. Arbitrary backend strings
remain private, and the fallback does not direct users to nonexistent diagnostics.

Generic JSON 503 failures emit one structured `object_backend_failure` error
event. Its cause preserves allowlisted storage/database failures while discarding
paths, SQL values and credentials; unrecognized causes remain withheld. The
backend regression captures actual stderr and verifies severity, one event per
failure, safe causes and secret redaction without changing the HTTP response.
Deleted admission-rejection test references are removed from the ledger, and
current resource contracts use `max_connections` instead of eight upload slots.
Historical evidence remains unchanged.

No benchmarks, real-cloud tests or container builds are part of this slice.
Broader DESIGN-070 and DESIGN-071 requirements stay PARTIAL. Existing request
deadlines still apply; a completed-body commit can finish after response timeout.
