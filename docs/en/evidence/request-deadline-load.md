# Request deadlines under CPU and filesystem contention

Investigated on 2026-11-11 on macOS arm64, using the pinned Rust 1.99.1,
edition 2024 and locked dependencies. This is one local diagnosis, not a
performance baseline, latency promise or regression gate. No Google Cloud
request, container build or SDK harness was run.
Reproduction and the temporary measurement used rusqlite 0.41.1 and
libsqlite3-sys 0.39.1. Final verification follows the integrated dependency
update to rusqlite 0.42.0 and libsqlite3-sys 0.40.0; the timings are not a
comparison between these dependency versions.

## Reproduction and cause

`server::batch::tests::a_part_past_the_request_limit_answers_alone_and_later_parts_still_run`
sets `request_timeout_ms=100`. It deliberately holds every gzip decoder, so
the first part must return its own 503. Its successor DELETE must still
return 204. Ten CPU-loaded serial runs and 36 parallel runs passed, but a
burst of 64 concurrent copies, while 28 CPU busy loops and eight concurrent
sync tests ran, failed 42 times at the original assertion:

```text
assertion failed: next_text(&mut body).await.contains("204")
```

The request limit is a Tokio monotonic-clock deadline. Both JSON and XML
DELETE awaited `catalog.delete_object` without the catalog-wait guard that
already protects uploads, rewrite and compose. A busy catalog thread or
filesystem could outlast 100 ms while committing the mutation or reclaiming
its old blob. The part watchdog then returned 503 and dropped its handler;
accepted catalog work and reclamation still ran. This is a production
response race, including ordinary single-request DELETE, rather than an
assertion that merely needs a larger timeout.

An independent deterministic regression held `Fault::Block` at
`ReclaimUnlink`, after the delete committed, and advanced Tokio time by one
second. Before the guard was added, the response was 503 while reclamation
was held. The same regression now requires a pending response until the
barrier is released, then 204 and a missing object. It exercises both JSON
and XML dispatch. Another paused-time test holds all catalog admission
slots, exhausts the catalog's own 1 ms budget, and requires 503 identifying
`catalog_wait_timeout_ms` with the object unchanged in both adapters.

`it_sync::sync_cli::workspace::live_cli_inspects_stopped_sparse_recovery_and_requires_exact_confirmed_discard`
uses a one-second wait immediately after listener readiness. Listener
readiness does not establish that the asynchronous mirror has finished
startup recovery or published its pause. The server's wait budget starts
after request preparation and includes command queueing, so its `Timeout`
before the mirror can answer `Paused` is a valid bounded-wait result.
The reported `Timeout` versus `Paused` failure did not reproduce in 54
CPU-loaded executions: six serial, two bursts of eight and one burst of
32. The test now observes the published `ConflictCapacity` pause through
the real status CLI before issuing its original one-second wait, including
after restart. It retains the exact reason, exit-status, authorization,
revision and retained-byte assertions. Production sync behavior is unchanged;
the status synchronization addresses the startup assumption found in the code,
without claiming a reproduced production sync defect.

## One temporary timing run

One instrumented run used 28 CPU busy loops and 64 concurrent copies of the
original batch test, with the JSON guard applied so all stages could finish.
All 64 passed. Temporary timestamps and prints were removed afterwards;
there is no benchmark harness. These are elapsed times, including operating
system scheduling delays, not CPU times or isolated syscall costs.

| Measured interval | Mean (ms) | Maximum (ms) |
| --- | ---: | ---: |
| DELETE catalog call, including admission and completion delivery | 261.762 | 530.875 |
| Mutation before SQL commit | 0.303 | 0.752 |
| SQL `transaction.commit()` | 239.914 | 496.631 |
| After catalog completion through awaited reclamation | 122.803 | 308.020 |
| Reclaim activity acquisition | 0.001 | 0.012 |
| Reclaim serialization permit | 0.001 | 0.049 |
| Reclaim catalog selection and task admission | 0.664 | 7.531 |
| Blocking-pool queue before filesystem closure | 0.134 | 3.820 |
| Filesystem closure | 53.499 | 119.685 |
| Blob unlink | 10.110 | 23.894 |
| Blob directory synchronization | 11.727 | 26.518 |
| Catalog call forgetting the deletion journal | 8.524 | 40.105 |

Nested intervals overlap and must not be added as independent costs.
The slowest SQL commit took 496.631 ms after only 0.300 ms of mutation work.
That request's blocking-pool queue was 0.015 ms, unlink 5.110 ms,
directory synchronization 10.704 ms and journal-forget call 6.480 ms.
The commit itself already exceeded the 100 ms request budget; moving only
blob reclamation after the response would not fix this race.

## Response ordering and retained limits

The DELETE transaction records synchronization changes, notification outbox
entries and the unreferenced-blob deletion journal with the logical deletion.
Commit must finish before success. The writer then signals consumers,
invalidates its read cache, and releases connection/execution ownership
before delivering completion. Cache invalidation must precede success so an
immediate read cannot receive the deleted cached object. Notification signals
do not wait for network delivery, and deletion does not wait for the mirror
to update its host tree.

The request currently also awaits one bounded reclamation page: select
unreferenced IDs, unlink their blobs, synchronize their directories, then
forget journal rows in another transaction. These steps can be deferred
without changing logical-delete visibility. The existing
[persistence design](../design.md#persistence-and-concurrency) deliberately
keeps a successful publication/deletion result when reclamation fails and
hands the retained journal to maintenance/recovery. Upload overwrite,
resumable completion and derived publication for rewrite/compose use the
same synchronous reclamation pattern; their adapters already guard the
catalog wait. JSON and XML DELETE now use the same `ctx.await_catalog` rule.

The separate catalog/object wait budgets still apply. If an operation is
still waiting for admission when its budget expires, cancellation prevents
it from starting. Once a filesystem or catalog job is accepted, exhausting
the catalog budget can leave an unknown mutation outcome; the job may still
commit. This existing upload/catalog contract is not changed to promise
rollback after cancellation. The new guard prevents the shorter request
inactivity budget from replacing that contract.

### Deferred reclamation follow-up

Moving ordinary reclamation after the response is a separate latency and
resource-lifetime change. It must explicitly wake maintenance for newly
journaled blobs even when no lifecycle rule is armed: `schedule_expiry`
currently wakes it for soft-deleted objects, while ordinary `Journaled`
retirement relies on immediate reclamation or its failure wakeup. It must
also reconcile memory-backend capacity release, admission and close/drain
ownership, restart/recovery, and every caller's unlink/sync/forget fault
tests. Preserve journal durability, bounded workers and reference checks.
The temporary 28-busy-loop/64-process measurements above motivate that
follow-up but establish no performance target. Neither that change nor
weaker synchronization is part of this fix.

## Neighbouring timing tests

- Batch part-expiry and enforced-object-deadline tests already use paused
  Tokio time. The blocked-admission test's 200 ms negative check cannot
  acquire a held permit, and its 1 ms timeout deliberately keeps that
  permit held; neither requires filesystem work to finish inside that bound.
- The batch connection-inactivity test holds every decoder while checking
  that a client read remains silent for 500 ms. Its 100 ms inactivity limit
  is the behavior being checked, not a deadline for a real filesystem
  operation to finish; releasing the decoder has no short success deadline.
- The three one-second `Paused`/incomplete assertions in the workspace test
  now follow the explicit startup-pause observation. The one-second CLI
  timeout in `sync_cli/ambiguous.rs` intentionally loses a response after
  the proxy has observed a committed restore; a timeout or closed connection
  is the expected negative result, and the committed generation is checked.
- Other sync waits use finite 5/10/30/60-second observation or socket/hang
  guards. They do not have the immediate one-second startup-pause assumption.
  They remain real-time integration guards, not deterministic latency
  guarantees. The startup interruption test also waits up to 10 seconds for
  the first exported file before interrupting a larger export; this separate
  scheduling assumption remains outside the two diagnosed failures.

## Verification

After rebasing onto the rusqlite 0.42.0 dependency update, these commands
passed on the final source:

```sh
cargo check --tests
cargo clippy --workspace --all-targets --locked --offline -- -D warnings
cargo test --lib --test it_sync --test it_protocol --test it_xml_auth --no-run
cargo xtask lint
cargo fmt --all --check
```

The emitted test binaries were then run serially, with loopback-listener
permission from the start:

```sh
target/debug/deps/gcs_mimic-5011867b69fb26eb server::batch::tests
target/debug/deps/gcs_mimic-5011867b69fb26eb server::xml_read::tests
target/debug/deps/it_protocol-2c1843055eeb7bfd http_batch
target/debug/deps/it_xml_auth-301e25fdb618e9eb xml_storage_http
target/debug/deps/it_sync-4b8d5cff03da88fe
```

Their results were respectively 15, 2, 13, 15 and 32 passed, with zero failed,
ignored or measured tests. The unit executable hashes are specific to this
native build; `cargo test --no-run` prints the paths for another build.
The production lint reported zero new violations and zero ratcheted
findings, and test wall-clock calls stayed within the unchanged baseline.
Cached markdownlint-cli2 0.24.0 / markdownlint 0.42.0 also checked this note:
`Summary: 0 issues in 0 files`.

Finally, two deliberate stress bursts each ran 64 copies of the original
batch test and 32 copies of the workspace sync test concurrently with 28
CPU busy loops on the ordinary filesystem. Both used the exact named test
filter and `--exact`, checked each process's exit status and nonempty
one-test result, printed failed logs if any, and terminated the busy loops
on exit. This keeps the busy-loop method of `scripts/stress-tests.sh` while
restricting repeated execution to the diagnosed tests. The captured summary
was:

```text
stress round 1: CPU loops=28; batch failed=0/64; sync failed=0/32
stress round 2: CPU loops=28; batch failed=0/64; sync failed=0/32
Summary: 0 failures in 128 batch and 64 sync executions
```

The original 100 ms request limit, one-second sync waits and all original
assertions remain active. The post-fix sync counts demonstrate that the
state-based setup passes under this load; they do not establish a measured
improvement over its unreproduced failure.

The full workspace suite, container builds, SDK harnesses and guide
walk-throughs are intentionally left to the integrated-source verification.
This repair touches the behavior covered by `BEHAVIOR-COMMON-008`,
`JSON-OBJECTS-004` and `EXT-XML-010`, and the sync test setup associated with
`DESIGN-072`; no feature-ledger status or supported subset is changed.
