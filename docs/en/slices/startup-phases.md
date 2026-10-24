# Shared startup phases

<!-- markdownlint-configure-file {"MD013": false} -->

Scope: DESIGN-062, DESIGN-072 and DESIGN-074, following the [shared onboarding contract](onboarding-state-contract.md). These broad rows remain PARTIAL; UX-002 retains its IMPLEMENTED seed-before-ready scope.

## Order and phase results

The `config-public-origin` slice appends `public_base_url` and
`public_virtual_host_enabled` to `ReadyEvent`; API-origin reporting remains
owned by `config-api-origin-default`.

Synchronous configuration, TLS and registry validation still precede runtime/signal registration. The runtime delegates to `startup::serve`; the phase bodies and their tests live in `src/startup.rs`. `open_catalog` drains recovery before `prepare_startup` calls PAP configuration, seed import and bind in one visible sequence, followed by maintenance startup and readiness. `PhaseResult` contains only Complete(value) or Stopped; signal failures use the outer io::Result. `BoundStartup` carries the bound listener and typed `SeedOutcome`. `ReadyEvent` serializes the existing ready fields, omitting `identities` in permissive mode. JSON keys now follow struct declaration order rather than alphabetical order; their names and values are preserved. Additional initializer/import/sync phases remain unimplemented; create-missing initialization must be inserted before seed with its own reviewed cancellation contract.

The private `startup::race` accepts only futures explicitly wrapped in the private `CancelSafe` marker and gives an observed shutdown signal priority. One pinned shutdown future covers the entire asynchronous startup and subsequent serving. An arbitrary compound startup future cannot be passed directly to race. No per-file tasks, polling scheduler, new setting or dependency is introduced.

CancelSafe is a nominal audit marker, not a compiler proof of cancellation safety: its constructor can wrap any future inside this module. A new phase must document every side effect's owner, permits and cleanup, add a production-path interruption test plus an ownership/effect test, and establish close-before-unlock before receiving this marker. Do not mark a new initializer/import/sync composite merely because adjacent phases are marked.

## Cancellation, failure and ownership

Open/recovery owns blocking filesystem and SQLite work. On interruption, startup awaits that same open future and explicitly closes any returned catalog before reporting stopped. Recovery internals, reclamation, schema and durability are unchanged. A recovery failure remains a nonzero startup failure without readiness.

PAP, seed and bind each have a separate marked wait. The marker's reviewed cancellation points are:

- Open/recovery: race borrows a pinned owner; cancellation drops only that borrow. The owner is then awaited and its catalog closed, including when the signal itself fails.
- PAP: before submission, dropping admission/execution waits changes no catalog state. After submission, Catalog::run owns the blocking SQLite job and its execution permit through completion.
- Seed: source reads and admission waits have no catalog side effects. Submitted metadata jobs own execution permits; submitted uploads own an upload permit and a cancellation channel through staging, publication and reference-aware cleanup. Dropping the seed request stops the walk and signals that upload, without dropping the owned operation.
- Bind: the socket is locally owned and TIME_WAIT is a side-effect-free wait; dropping it releases the socket.

Startup emits stderr `{"event":"stopping"}` when an asynchronous pre-bind shutdown is accepted, then closes admission and waits for owned permits before releasing the directory lock. It is an acknowledgement, not a claim that close has completed. New initializer/import/sync phases are not implicitly marked safe by a surrounding composite race. The startup driver routes cancellation, PAP/seed/bind failure and listener-address/ready-serialization failure through one close/finish path. Stopped output has one definition. Both startup and the running server use diagnostics::combine_shutdown_results: a close failure prevents stopped, preserves the primary error and includes close's I/O kind/cause with repair advice.

Stopped means catalog ownership has been released. Shutdown during recovery waits for recovery to finish without a deadline; a second signal does not accelerate that drain. Object-count-proportional recovery is unchanged. The measured startup-100000 wall median is 0.777482 seconds (range 0.727584–1.737045); these startup observations are not an upper bound on shutdown latency. SQLite work may finish or reach its existing timeout first. Committed seed objects survive and retries remain create-if-absent. This slice does not implement administrative publication handles, new ambiguous-commit handling or background recovery; those broader acceptance cases remain PENDING.

## Acceptance evidence

| Case | Evidence |
| --- | --- |
| startup-phase-order | Existing foundation/seed suites preserve ready fields, strict seed API permissions, restart behavior and probe gating. `process_shutdown_reports_stopped_and_releases_catalog_for_restart` checks ready fields; `probes_cannot_succeed_while_seed_publication_is_blocked` checks bind follows seed. |
| startup-phase-cleanup | `shutdown_during_recovery_drains_open_before_unlocking` uses a clock barrier inside actual recovery. `shutdown_during_policy_configuration_drains_owned_work` calls production prepare_startup with fixtures/nonempty projects, interrupts after PAP submission and verifies seed stays unrun. `cancelled_policy_request_still_executes_its_owned_job` observes that the queued job replaces old projects with the exact new set in the same TEMP connection after request cancellation. External SQLite write locks do not block this TEMP policy table, and reopening intentionally loses it. |
| failure and signal results | `startup_failures_close_catalog_even_with_live_handles` drives PAP's 65-project rejection through prepare_startup, checks source/key diagnostics and tests bind failure with retained catalog handles and immediate reopen. `close_failure_preserves_primary_diagnostics_and_unstructured_errors` injects close failure with successful, structured-error and plain-error inputs. Signal/error propagation and priority retain their named tests. |
| seed interruption | `sigterm_during_seed_drains_writes_releases_lock_and_retries` stops a pending SQLite seed write and verifies lock retention after stopping acknowledgement. `sigterm_during_seed_body_and_precommit_cleans_only_uncommitted_files` observes partial staging of a 64 MiB fixture and, separately, a renamed blob behind an external SQLite write lock, then sends SIGTERM and observes stopping. It requires only stopped on stdout, exit 0, empty staging and exact equality of blob files and committed references before reopening. The partial-transfer case explicitly requires zero objects/references; the post-rename case requires lock retention after acknowledgement and a completed owned commit whose bytes remain readable. Both retry successfully with a bounded range read. |
| bind interruption | Foundation's `process_restarts_on_same_port_without_allowing_a_second_live_listener` covers TERM/INT during BSD TIME_WAIT; bind exclusivity remains unchanged. |
| startup-phase-benchmark | Five native repetitions at 10,000/100,000 objects and frozen baseline comparison: [startup evidence](../evidence/startup-phases.md). |

The runtime and deterministic cleanup tests were run natively on macOS. Windows/Linux execution, container/SDK journeys, host power loss, cgroup memory, cold cache and a bounded recovery duration are not established by this slice. Recovery and existing API wire semantics were deliberately left unchanged.
