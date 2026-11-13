# Lifecycle storage class transitions

<!-- markdownlint-configure-file {"MD013": false} -->

Scope: `BEHAVIOR-LIFECYCLE-003` and the storage-class portions of `BEHAVIOR-LIFECYCLE-001`, `BEHAVIOR-LIFECYCLE-002` and `BEHAVIOR-LIFECYCLE-008`. The feature ledger owns status. Supported transitions execute through the shared lifecycle sweep; the affected execution rows remain `PARTIAL` and target `LOCAL_SIMULATION`, which they achieve; `BEHAVIOR-LIFECYCLE-001` validates configuration with `EXACT` fidelity.

## Contract

An eligible transition updates the existing generation in place: `storageClass`, `updated` and `timeStorageClassUpdated` change, metageneration increases once, and the metadata-derived ETag changes. Creation time, generation, blob identity, size, checksums and body bytes remain unchanged. The recorded mutation time is no earlier than the pass time, creation time or previous `updated`. JSON and gRPC v1/v2 expose the stored class-change time; unrelated metadata changes preserve it. Shared publication resets class-change time to the new generation's creation time; upload, copy, rewrite, compose, restore and move exercise this behavior.

The supported transitions are:

| Source | Destinations |
| --- | --- |
| `DURABLE_REDUCED_AVAILABILITY` | `MULTI_REGIONAL`, `NEARLINE`, `COLDLINE`, `ARCHIVE` |
| `STANDARD`, `MULTI_REGIONAL` | `NEARLINE`, `COLDLINE`, `ARCHIVE` |
| `NEARLINE` | `COLDLINE`, `ARCHIVE` |
| `COLDLINE` | `ARCHIVE` |
| `ARCHIVE` | None |

Unlisted or identical transitions are accepted but inert. `MULTI_REGIONAL` is eligible only in multi-region or dual-region buckets. Unknown location kinds do not transition; this defensive catalog check does not establish zonal-bucket support. `REGIONAL` and `RAPID` object/action classes remain unsupported. Autoclass validation rejects transition rules and `matchesStorageClass` without changing stored rules or objects. Enabling Autoclass after removing rules updates the class time of changed live/noncurrent classes; soft-deleted and already-Standard classes retain theirs.

Selection uses current rules and facts at the physical pass time. A matching Delete without `matchesStorageClass` wins. If every matching Delete has a storage-class condition and an applicable transition exists, the transition wins instead. Among transitions, the coldest eligible supported destination wins regardless of rule order. Applicable means its conditions match now and the transition table and bucket location permit an actual class change. Inert or future transitions cannot defer Delete. After a class change, the same transaction re-evaluates the new class at the same time: a Delete still matching that class runs immediately.

This is an explicit local priority difference. [Google documents Delete priority when multiple actions match simultaneously](https://docs.cloud.google.com/storage/docs/lifecycle). gcs-mimic lets a class-dependent Delete yield even when passes run on time. The current record cannot distinguish a late transition from the intended class history. This rule protects the common case of Nearline at age 30 plus Standard-only Delete at age 60: a first pass at age 100 transitions to Nearline and retains the object. It does not establish that every delayed result differs only by retaining objects.

Rules added to an aged object are evaluated immediately against its current state. Custom-Time is effective immediately, including backdated values on upload or PATCH; unrelated metadata updates do not hide it. Existing noncurrent timestamps remain unchanged, and live-only rules never match noncurrent versions. No rule history, historical condition ordering or additional persistent state is reconstructed.

Versions are scanned in descending `(name, generation)` primary-key pages. Newer-version conditions count currently surviving, non-soft-deleted successors, including the live version. A same-pass removal of a successor therefore changes the count seen by an older generation. A live Delete in a versioned bucket records physical `timeDeleted` and stops processing that generation. Noncurrent actions begin on a later pass, even for zero noncurrent days. Notification, synchronization and soft/hard-delete times also use the physical clock.

Every transition, archive or removal consumes one of the 1024 actions allowed per pass. The strict cap can stop partway through a generation's class chain. Continuation returns the current pass time and resumes from the stored class in a later pass; completed transitions do not repeat. Each page commits atomically, but the cap does not promise a whole-generation chain. The supported class graph is acyclic.

Holds and retention permit class transitions. A selected protected independent Delete blocks transitions; removing a hold invalidates the schedule and retention schedules its expiry. A class-dependent Delete that yields has not been selected, so it cannot block the transition. A future transition that can win over a blocked class-dependent Delete supplies a wakeup deadline, including before retention expiry. Live and noncurrent generations transition; soft-deleted generations do not.

Class updates, metadata journal entries and `OBJECT_METADATA_UPDATE` payloads commit together. Failed UPDATE or commit rolls back the page without partial events and remains retryable. Process-abort and power-loss snapshots restart without duplicate transition events. No new filesystem write, rename or synchronization step is introduced. Malformed stored class timestamps use the existing version quarantine while healthy versions continue.

Execution is enabled by default without `sync_dir`, disabled by default with it, and explicit `enabled`/`disabled` overrides either default. With synchronization enabled, class-only journal entries do not re-export body bytes or change the host file's inode, modification time or contents. A matching Delete can still remove the file. The guarded run route reports `transitioned`; `scanned` counts a visited generation once even when several actions run.

## Delayed-pass differences

On-time passes use instantaneous priority, with the explicit class-dependent Delete exception above. A delayed pass sees only current candidates, so the path through storage classes can differ. Results can differ in either direction. These three deletion-side forms are fixed by paired timely/delayed tests:

| Form | Concrete example | Timely versus delayed result |
| --- | --- | --- |
| A successor disappears before an older transition is evaluated | Versions created at days 0, 5 and 10; Nearline requires age 15 and two successors; Standard-only Delete requires 20 noncurrent days. Execution stops after day 10 and resumes at day 50. | Timely execution transitions the oldest at day 15. Delayed descending execution removes the second version first, so the oldest loses its transition evidence and is also deleted. |
| An overwrite removes a live-only opportunity | Nearline requires live age 30; Standard-only Delete requires one noncurrent day. A version created at day 0 is overwritten at day 40; delayed execution resumes at day 45. | Timely execution leaves the old version in Nearline. Delayed execution sees it as noncurrent, cannot apply the live-only transition and deletes it. |
| Class-gated transitions take different paths | Nearline at age 10 and Coldline at age 20 both require Standard; Delete at age 30 requires Coldline. The first delayed pass runs at day 30. | Timely execution enters Nearline at day 10 and no longer qualifies for the Standard-to-Coldline rule. Delayed execution chooses Coldline directly, then deletes. Other class-path divergences are the same form. |

A retention-side example uses Nearline at age 30, Coldline at age 90 requiring Nearline, and Delete at age 60 requiring Nearline. Timely execution deletes at day 60; a first pass at day 100 applies both transitions and retains Coldline. Simultaneous class-dependent Delete/transition matches can also retain an object that documented Google priority would delete.

If Delete's `matchesStorageClass` also includes the destination class, the same pass transitions and then deletes, including on-time passes: the transition adds a metageneration increment and metadata-change record, and the two actions consume two slots in the pass budget.

The bounded safety test evaluates one version with fixed non-class facts and terminal Delete; versioned archiving and changing successor evidence have separate catalog tests. Its safe domain is defined exactly as rule sets whose SetStorageClass rules have no `matchesStorageClass`. Every generated set in that domain is included, with no individual counterexample exclusions. It checks that delayed deletion implies deletion under an independent, unmodified Delete-first oracle run on a daily grid. This bounded result is not a general guarantee for arbitrary rule histories or multiple changing versions.

## Upgrade and rollback

Catalog schema 53 adds nullable `objects.time_storage_class_updated`. Schema 52 records migrate with NULL and read creation time until a transition occurs. Older serialized records use the same fallback. Reopening preserves transitions and class times. Existing released-data fixtures preserve metadata and bodies; a synthetic schema 52 fixture verifies stored rules, including a first pass after both transition and old-class Delete deadlines. Released fixtures themselves contain no lifecycle rules.

Back up the complete stopped data directory before opening it with this version. Opening upgrades the catalog even when execution is disabled. Older binaries supporting schema 52 reject schema 53; rollback requires restoring the pre-upgrade backup. Execution can change classes and activate previously deferred class-dependent Delete rules immediately. Set `lifecycle_execution=disabled` before startup when those effects are not wanted.

## Evidence

Official sources rechecked on 2026-11-11: [lifecycle](https://docs.cloud.google.com/storage/docs/lifecycle), [metadata](https://docs.cloud.google.com/storage/docs/metadata), [holds](https://docs.cloud.google.com/storage/docs/object-holds), [bucket retention](https://docs.cloud.google.com/storage/docs/bucket-lock), [object retention](https://docs.cloud.google.com/storage/docs/object-lock), [Pub/Sub notifications](https://docs.cloud.google.com/storage/docs/pubsub-notifications), [Rapid buckets](https://docs.cloud.google.com/storage/docs/rapid/rapid-bucket) and [Autoclass](https://docs.cloud.google.com/storage/docs/autoclass). The [research](lifecycle-execution-research.md) records pinned Discovery revision 20261103, SHA-256 `30b498129041aceedca2e60678443e2671531cf0c08b3488c6934c212a2cb272`, and the Storage v2 protobuf revision and digest.

Metageneration increment, class timestamps and metadata notifications follow general metadata documentation; lifecycle-specific behavior remains inferred without provider observations. Same-pass chaining, protected-Delete behavior and asynchronous timing are local simulations. The Autoclass bulk update still leaves object `updated`, metageneration and notifications unchanged, so its class time can exceed `updated`; this remains an explicit gap. No Google Cloud request was made. Propagation delay and billing are not simulated; the event-hold-release retention-age limitation remains unchanged.

| Acceptance | Executed test evidence |
| --- | --- |
| Full represented transition table and exact age boundaries | `every_supported_transition_and_inert_pair_obey_exact_age_deadlines` |
| In-place metadata, ETag, stable bytes and no repeated transition | `a_selected_storage_class_action_updates_metadata_once` |
| Location limits and Autoclass configuration rejection | `location_incompatible_transitions_are_stored_but_inert`, `autoclass_rejects_transition_configuration_without_changing_objects_or_rules` |
| Current priority, inert/future targets, independent Delete and same-pass re-evaluation | `a_class_dependent_delete_yields_only_to_an_applicable_transition`, `an_independent_delete_wins_even_when_a_class_dependent_delete_also_matches`, `a_delete_matching_the_resulting_class_still_runs_in_the_same_transaction` |
| Late first pass and disabled-to-enabled execution | `a_late_first_pass_transitions_before_an_old_class_delete`, `enabling_or_replacing_rules_evaluates_current_class_candidates` |
| Current rules, immediate Custom-Time and unchanged noncurrent history | `newly_configured_rules_choose_current_candidates_without_replaying_old_deadlines`, `newly_configured_rules_preserve_the_observed_noncurrent_time`, `backdated_custom_time_is_effective_immediately_on_upload_or_patch` |
| Live-only rules and protection after overwrite | `live_only_rules_never_match_existing_noncurrent_versions_even_after_versioning_is_suspended`, `a_noncurrent_transition_cannot_enable_a_historical_live_only_rule`, `an_overwritten_retained_version_stops_matching_a_blocked_live_delete` |
| Hold/retention wakeups and class-dependent yielding | `protected_class_deletes_schedule_a_future_transition_before_retention_expires`, `releasing_a_hold_preserves_custom_time_and_only_class_dependent_delete_yields`, `a_class_dependent_delete_yields_to_further_transitions_during_retention` |
| Delete still matches the transitioned class under protection | `retention_blocks_delete_matching_a_transitioned_class_until_expiry`, `a_hold_blocks_delete_after_transition_and_waits_for_the_next_transition` |
| Three known deletion-side differences and one retention-side difference | `a_delayed_pass_can_delete_an_old_version_after_removing_its_transition_evidence`, `an_overwrite_during_a_stop_can_remove_a_live_only_transition_opportunity`, `a_delayed_coldest_transition_can_take_a_class_path_that_enables_delete`, `a_delayed_pass_can_keep_a_version_that_an_earlier_class_delete_would_remove` |
| Descending scans and physical archive time | `descending_scans_remove_successors_before_testing_an_older_delete_threshold`, `a_delayed_archive_starts_noncurrent_age_at_the_physical_pass_time`, `zero_noncurrent_days_delete_on_the_pass_after_archiving` |
| Strict cap and bounded class chain | `each_chained_action_counts_toward_the_pass_limit_and_resumes_next_pass`, `a_tiny_action_budget_stops_mid_chain_and_resumes_from_the_stored_class`, `supported_class_transitions_form_an_acyclic_graph` |
| Independent priority oracle and explicit safety domain | `delayed_deletion_safety_has_an_explicit_class_independent_transition_domain` |
| Monotonic class times, new-generation resets and Autoclass | `transitions_never_backdate_creation_or_a_newer_metadata_update`, `every_new_generation_starts_its_storage_class_time_at_creation`, `enabling_autoclass_updates_the_class_time_only_for_changed_live_and_noncurrent_versions` |
| UPDATE/commit rollback, crash and power-loss recovery | `failed_transition_update_rolls_back_the_page_and_retries`, `failed_transition_commit_preserves_metadata_events_and_journal_for_retry`, `transition_crash_and_power_loss_restart_converge_without_rewriting_or_duplicate_events` |
| Quarantine, synchronization and execution settings | `malformed_class_timestamp_is_quarantined_while_healthy_transitions_finish`, `synchronized_transitions_follow_execution_settings_without_exporting_body_bytes` |
| Schema 52 migration, late stored rules and old serialized records | `schema_52_migrates_null_class_times_and_executes_stored_rules`, `upgrading_stored_rules_after_their_deadlines_transitions_before_old_class_delete` |
| Protocol timestamps | `json_outputs_the_stored_lifecycle_storage_class_timestamp`, `grpc_v1_outputs_the_stored_lifecycle_storage_class_timestamp`, `grpc_v2_outputs_the_stored_lifecycle_storage_class_timestamp` |

The independent oracle exhausts 7750 unordered pairs with replacement from 124 rules (31 condition atoms/conjunctions times four actions), plus 364 triples from 12 representative rules. Six fixed fact snapshots cover live/noncurrent state, absent/current/past Custom-Time and zero/one/two successors. A daily grid runs from day 0 through 40, with delayed comparisons at days 1, 10, 20, 30 and 40. The full condition product is pruned to atoms and temporal/class conjunctions; triples focus on class paths and Delete interactions. The baseline never calls the production evaluator, eligibility helper or transition selector.

The 98,400 comparisons with class-independent transitions contain no delayed-only deletion. Outside that domain, 137,700 pair comparisons contain none and 7320 triple comparisons contain 32; all 32 visit a class absent from the timely path. New-rule versus original-priority timely execution differs in 1515 pair comparisons and zero triple comparisons; every difference originates in simultaneous class-dependent Delete and transition selection. Counts are fixed assertions. One local run completed in 0.59 seconds; this is a test result, not a benchmark gate.

Verified on 2026-11-11:

| Command | Result |
| --- | --- |
| `cargo check --locked --offline --tests` | Passed |
| `cargo fmt --all --check` | Passed |
| `cargo test --locked --offline --lib lifecycle -- --test-threads=4` | 174 passed, 0 failed |
| `cargo test --locked --offline -- --test-threads=4` | 3103 passed, 0 failed, 1 existing ignored local Pub/Sub opt-in test; includes 2021 library tests and all integration/documentation suites |
| `cargo xtask lint` | 0 new production violations, 0 ratcheted findings |
| `cargo xtask compatibility render` and `cargo xtask compatibility check` | 607 rows validated; generated English/Japanese matrices match |
| `just check-markdown` | CLI 0.24.0: 383 Markdown files and 2 generated views checked, 0 issues |

SDK, container, guide and real-Cloud checks are not part of this slice's local verification. Existing condition-only edge evidence is not promoted into transition execution coverage without a corresponding sweep test.

The full suite uses four test threads; default-concurrency behavior was not rechecked for this change.

Temporarily removing the post-transition blocked-count accumulation makes both protection regressions fail with `blocked=0` instead of `1`; restoring it passes the targeted suite.
