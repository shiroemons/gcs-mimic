# Local Pub/Sub notification delivery evidence

<!-- markdownlint-configure-file {"MD013": false} -->

Verified 2026-10-30 on macOS ARM64, Rust 1.99.1, with the committed Cargo graph
plus Hyper's `client` feature and its locked `want`/`try-lock` dependencies.
Contract: [local Pub/Sub notification delivery](../slices/pubsub-notification-delivery.md).
Feature scope: EXT-BEH-032, EXT-BEH-033 and JSON-NOTIFICATIONS-001 through
JSON-NOTIFICATIONS-004. These observations do not promote the broader ledger
rows or establish real Google service compatibility.

## Commands and results

| Command | Result |
| --- | --- |
| `cargo check --locked --offline --tests` | PASS without warnings on the integrated source. |
| Final `scripts/test-rust.sh` workspace run | PASS: 60 binaries, 1674 passed, 0 failed, 1 explicitly opted-in Pub/Sub test ignored; doc tests add 4 passed. The 924-case library includes publisher acknowledgement, retry/recovery and configuration cases. See [combined evidence](migration-integration.md) for the exact command. |
| `notifications_http` in the final workspace run | PASS: 6 passed, 0 failed, 1 explicitly opted-in test ignored, 0 filtered out. This includes legacy global notification rules publishing alongside JSON-configured notifications. |
| `cargo xtask lint` | PASS: 0 production violations; 1 existing reasoned suppression. No notification suppression was added. |
| `cargo fmt --all --check` and workspace all-target clippy with `-D warnings` | PASS. |
| Explicit official-emulator opt-in | PASS: 1 passed, 0 failed, 0 ignored, 6 filtered out. The previously ignored default test was executed separately with the configured local endpoint. |
| Shared HTTP framing helper consumers | PASS: 17 targets, 156 passed, 0 failed, 1 opt-in ignored in the default group. Test check and clippy also pass. HEAD has no body; Content-Length responses are read to their stated length instead of connection EOF. |

Local TCP listener tests ran with permission to bind loopback sockets. All
default tests remained offline; no Google Cloud or metadata endpoint was used.

The final configuration cases distinguish native backend validation from
unsupported legacy settings. Notification negatives retain malformed/oversized
acknowledgement, redirect, service failure, backpressure and recovery coverage.

## Observed behavior

| Acceptance | Evidence |
| --- | --- |
| Four ordinary object transitions and overwrite relationships | `finalize_archive_delete_snapshots_survive_restart_and_failed_mutations_emit_nothing` observes finalize, archive, finalize, archive and permanent delete, with old/new generation attributes and unchanged failed-precondition state. |
| Metadata snapshot, prefix/event filtering, custom attributes and payload formats | `metadata_payload_filters_and_deleted_configuration_use_transaction_time_snapshots` compares encoded resulting metadata, checks `NONE`, standard attribute precedence and queued snapshots after configuration deletion. |
| HTTP mutation and CRUD effects | `notification_effects_follow_http_mutations_and_persist_without_a_publisher` verifies logical decoded names, failed PATCH/DELETE preconditions, overwrite/delete events and unchanged queued data after process restart. Existing notification CRUD and strict permission tests also pass. |
| Legacy global settings and JSON notifications both have delivery effects | `legacy_global_notification_rules_and_json_configs_publish_to_the_explicit_local_endpoint` captures both publish requests at a loopback HTTP receiver, checks topic paths, independent filters, payloads, custom attributes and absence of credentials. |
| Queue backpressure preserves mutation state | `full_outbox_rolls_back_the_object_mutation` fills 4096 rows, verifies no object, generation allocation or object ACL survives the rejected write, then frees a row and publishes successfully. `byte_budget_and_count_budget_both_rollback_the_mutation` independently fills the 16 MiB byte budget and verifies the same failure/recovery behavior. |
| Durable publication and queued events share one commit | `publication_failures_and_crash_snapshots_keep_objects_and_events_atomic` injects failed body write, flush, sync and commit, then restarts; process-abort snapshots before/after commit agree on object and event presence. |
| Power-loss recovery | `power_loss_keeps_publication_and_outbox_in_the_same_commit` reopens modeled power-loss snapshots before publish, after publish, before commit and after commit; only the committed object has a queued event. |
| At-least-once replay | `missing_acknowledgement_retries_the_same_snapshot_after_restart` loses the publish response, records retry state, restarts and observes identical topic/message snapshots before acknowledgement. |
| Retry scheduling and close | `retry_deadline_allows_other_messages_and_close_waits_for_the_active_attempt` defers the first message, publishes the second, proves close waits for the network attempt and preserves unacknowledged snapshots after restart. |
| Reset isolation | `reset_is_busy_during_network_attempt_and_clears_retry_payloads_afterward` observes busy during an active attempt, succeeds during backoff, rejects a subsequent stale send and reopens an empty queue. |
| Migration and corrupt accounting | `schema_migration_preserves_configs_and_inconsistent_accounting_fails_read_only` migrates schema 37 to 38 while preserving configuration effects, then verifies read-only rejection of inconsistent queue counters. |
| Origin guard and topic escaping | Publisher tests reject public/Google/metadata origins, mapped IPv6 and alternate public IPv4 notation; reserved characters in project/topic IDs are escaped in the actual HTTP request. |
| Configuration | Notification configuration tests verify incomplete-setting failures, event selection and deterministic native-versus-standard environment precedence through both environment readers. |

## Official local-emulator opt-in

The explicit opt-in ran against the official Cloud SDK 591.0.0 component
manifest revision `20261026202537`, Pub/Sub emulator `0.8.37`, component build
`20261009074343`. The official archive is
[`google-cloud-sdk-pubsub-emulator-20261009074343.tar.gz`](https://dl.google.com/dl/cloudsdk/channels/rapid/components/google-cloud-sdk-pubsub-emulator-20261009074343.tar.gz),
53,022,061 bytes, SHA256
`28f872a91407ff2a41e29fcf79567f9238322451ad4eb18fa541086ee9982fcf`.
Its emulator JAR SHA256 is
`6a57355f6b6a8596ef0ffee91d377b034b24319724a33a9fc649e2e25b898791`.
The fixture uses JDK 27.0.1 and an explicit loopback listener, without Google
credentials or a real Cloud project.

```sh
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=2 \
  GCS_MIMIC_TEST_PUBSUB_EMULATOR_HOST=127.0.0.1:58741 \
  bash scripts/with-test-tmpdir.sh cargo test --locked --offline \
  --test notifications_http opted_in_pubsub_emulator_receives_the_notification \
  -- --ignored --exact
```

The test creates an isolated topic and subscription, commits an object through
the Rust API, receives its outbox publication, pulls the message and checks
`OBJECT_FINALIZE` and the object ID before deleting the local resources.
The emulator's persistent Content-Length response exposed an EOF assumption in
the shared test helper; honoring the stated body length resolves the fixture
failure without changing the publisher, SDK, timeout or acknowledgement checks.
The command exits zero in 1.15 seconds. The local emulator was stopped after
cleanup. Its IAM-not-supported, SLF4J-provider and deprecated-Unsafe warnings
remain visible and do not establish cloud IAM behavior.

## Unverified boundaries and next checks

- The new memory backend's main-catalog budget is independent of the outbox
  byte cap and may fill first. Notification tests here use the disk backend;
  metadata-heavy failure preservation and deletion/reset under a full memory
  catalog require that backend's integration checks.
- Queue-full object creation is covered; metadata-heavy updates and reset of a
  completely full catalog are not established by these tests.
- Provider-exact soft-delete event timing, Rapid `OBJECT_INITIALIZE`, automatic
  lifecycle execution, cloud service-agent permission checks, Windows delivery,
  real Google publication and hardware power failure remain outside this
  evidence.
- Container/official SDK checks remain separate from the completed native
  workspace run. No performance or memory-usage improvement is claimed.

The canonical feature ledger and generated matrices require a separate evidence
reconciliation that preserves these pending boundaries. Existing historical
notification CRUD evidence remains valid for that CRUD subset.
