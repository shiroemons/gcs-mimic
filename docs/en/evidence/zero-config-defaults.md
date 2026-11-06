# Zero-configuration defaults verification

<!-- markdownlint-configure-file {"MD013": false} -->

Recorded **2026-11-06** for the behavior the [0.12.0 notes](../releases/0.12.0.md) describe, at code commit `6a1794f9`. A migration check found that a working setup needed added settings for connections, object size, timeouts, soft delete and the advertised URL. The defaults changed so that `GCS_MIMIC_SCHEME=http` and the data mounts are enough.

## Sources for the timeouts

Cloud Storage documents no seconds for waiting on a request head, silence on a body or a whole request, so those defaults are local choices. The pages and what they say are in [the source record](../sources.md#timeouts-and-limits-behind-the-local-defaults).

## Rust tests

Each command ran through `bash scripts/with-test-tmpdir.sh`, with the pinned toolchain and no failures.

| Command | Result |
| --- | --- |
| `cargo test --locked --lib -- soft_delete catalog::connection::tests resumable_http_tests resumable::admission::tests resumable::writers resumable_lifecycle_tests config:: time_limit fd_limit reclaim_tests notification_outbox upload_replay xml_multipart grpc::tests catalog::rewrite catalog::derived url::tests session_limits server::transport deadline_claim` | 386 passed |
| `cargo test --locked --lib -- management_host_is_not_admitted consecutive_failed_passes a_tree_past_the_former_limits grpc_missing_preface seed::tests server::admin::guard task::tests` | 39 passed |
| `cargo test --locked --test it_protocol -- request_host_http phase_deadlines upload_admission` | 24 passed |
| `cargo test --locked --test it_json -- bucket_soft_delete_http` | 11 passed |
| `cargo test --locked --test it_objects -- rewrite_http` | 27 passed |
| `cargo test --locked --test it_process -- unix_transport foundation tls grpc_foundation log_level` | 57 passed |
| `cargo test --locked --test it_admin -- memory_backend admin_http` | 39 passed |
| `cargo test --locked --test it_import_seed -- bucket_initialization seed` | 64 passed |
| `cargo test --locked --test it_xml_auth -- xml_multipart sign_url` | 13 passed |

These tests set Host directly and do not start containers. The full `just check`, the SDK suites and the container smoke test belong to the release gate and are not recorded here.

## What the tests establish

| Behavior | Tests |
| --- | --- |
| Links follow Host and the connection scheme without an explicit base | `every_rendered_link_follows_the_host_of_its_request`, `advertised_scheme_follows_the_connection_and_forwarded_headers_do_not_apply`, `strict_mode_links_follow_the_request_host_without_an_explicit_base`, `unix_links_use_http_and_the_request_host_without_an_explicit_base` |
| A Host value decides nothing | `management_host_is_not_admitted_by_a_base_derived_from_the_same_host`, `management_routes_refuse_a_host_that_only_the_derived_base_would_name` |
| An explicit base and unusable hosts | `an_explicit_base_ignores_the_host`, `forwarded_headers_and_unusable_hosts_never_reach_an_advertised_link`, `an_xml_virtual_host_advertises_its_configured_domain_as_the_api_base`, `runtime_external_url_fixes_the_base_and_a_public_host_alone_does_not` |
| Retries and repeated links across addresses | `a_session_opened_through_one_host_completes_through_another`, `replayed_uploads_render_links_for_the_retrying_host`, `a_metadata_write_may_repeat_links_read_through_another_host` |
| Soft delete off by default | `unconfigured_native_server_frees_overwritten_and_deleted_bodies_at_once`, `every_backend_defaults_soft_delete_off_unless_the_setting_is_explicit`, `lowering_the_default_keeps_the_stored_policy_of_existing_buckets` |
| Any retention and expiry without a request | `short_retention_keeps_lists_and_restores_a_deletion_then_frees_its_body_when_due`, `the_longest_retention_gives_a_representable_hard_delete_time`, `one_pass_clears_a_disk_backlog_beyond_every_batch_limit`, `crash_at_every_expiry_boundary_recovers_without_losing_the_deletion` |
| Catalog callers wait, and a disconnect gives up the place | `callers_past_the_queue_wait_and_every_read_and_write_completes`, `a_dropped_caller_gives_up_its_place_without_running_or_consuming_a_slot`, `requests_past_the_catalog_slots_wait_and_all_succeed`, `a_disconnected_client_stops_waiting_and_its_change_never_runs` |
| A busy session waits | `a_retry_waits_for_the_abandoned_chunk_and_then_resumes_the_session`, `a_writer_that_keeps_working_past_the_budget_reports_a_busy_session`, `a_conflicting_lease_waits_for_the_holder_and_fails_only_past_its_budget` |
| No fixed session caps | `sessions_past_the_former_caps_survive_reopening_and_stay_usable`, `sessions_past_the_former_cap_survive_a_restart_and_stay_usable`, `uploads_past_the_former_limit_survive_reopening_and_a_held_one_does_not_stall_expiry`, `abandoned_rewrites_past_the_former_quota_survive_a_restart_and_can_finish`, `transfers_past_the_former_quota_survive_reopening_and_recovery_pages_through_them` |
| Windows drop their oldest entry | `the_oldest_finished_record_past_the_window_goes_first`, `a_retry_past_the_full_window_publishes_again_and_recent_retries_still_replay`, `writes_past_the_limit_succeed_oldest_first_and_the_newest_are_delivered`, `a_rejected_write_keeps_the_messages_it_would_have_evicted` |
| Each connection phase has its own limit | `obj_http_stalled_upload_body_outlives_header_and_request_deadlines`, `obj_http_stalled_download_outlives_header_and_request_deadlines`, `obj_http_explicit_body_inactivity_answers_a_stalled_upload_with_408`, `connection_timeout_alias_fills_every_setting_it_replaces`, `an_explicit_timeout_beats_the_alias_from_any_source`, `grpc_missing_preface_is_bounded_by_the_handshake_deadline_alone` |
| A full server closes one idle connection per waiting client | `full_server_closes_only_the_oldest_idle_connection_per_waiting_client`, `full_server_without_a_waiting_client_keeps_its_idle_connections`, `a_request_wakes_only_the_connection_that_turned_idle_first` |
| Capacity and the open-file limit | `capacity_settings_accept_their_raised_bounds_only`, `startup_raises_a_low_soft_open_file_limit_for_its_connections`, `startup_warns_when_the_hard_limit_keeps_connections_short_of_descriptors`, `accepted_tcp_connections_enable_keepalive` |

## Measurements

One run of each scenario on the integrated release build, native on macOS arm64, using a scenario written for this check, not a benchmark harness. These are facts about that run and not gates. The 0.11.0 comparisons used the same scenarios.

| Scenario | Result | 0.11.0 |
| --- | --- | --- |
| Go SDK, 32 parallel, 800 uploads and read-backs | 0 failures | 17 to 22 failures |
| 96 parallel GETs of 4 MiB, 480 requests | 0 failures | 0 failures, queued behind its 32 connections |
| Mixed load, 48 parallel, 15 seconds | 27,444 operations, no 503 or connection error, peak RSS 38 MiB | 449 responses of 503 and many connection errors |
| 300 resumable uploads left unfinished | All succeed and continue after a restart | 503 from the 129th |
| A write that stalls for 8 seconds and for 70 seconds | Succeeds | Not recorded |
| 3 GiB POST and GET | Contents match, peak RSS 17 MiB | Not recorded |
| 300 MiB overwrite and delete | Space reclaimed at once | About 615 MiB stayed on disk |
| `kill -9` twice | No loss among 787 acknowledged writes | Not recorded |
| 1000 transfers in flight, half uploads and half downloads | About 230 MiB RSS while held, peaks of 266 to 343 MiB | Not recorded |

Ten more runs of the last scenario had no failures. With the CPU saturated (load average above 100), a thousand clients connecting at once sometimes saw `connection reset by peer` right after connecting, and the server logged only `connection failed` for those. macOS caps the listen backlog at `kern.ipc.somaxconn`, 128 here, which is the likely cause.

### In the container

The final image on Docker Desktop for macOS arm64, using a Compose service with only `GCS_MIMIC_SCHEME=http` and mounts at `/data` and the data volume. Memory is what `docker stats` reports, which includes page cache and socket buffers, so it is not comparable with the native RSS above.

| Scenario | Result |
| --- | --- |
| Seed from `/data` | The bucket and object exist, and the bucket has no `softDeletePolicy` |
| Links | A client on `localhost:<published port>` and a peer on `gcs:4443` each got their own address, and each finished a resumable upload |
| Go SDK, 32 parallel, 800 uploads and read-backs | 0 failures |
| 96 parallel GETs of 4 MiB, 480 requests | 0 failures |
| Mixed load, 48 parallel, 15 seconds | 15,064 operations, no 503 or connection error, 53 MiB |
| 300 resumable uploads left unfinished | All succeed and continue after a restart |
| 5200 resumable uploads finished, and 5000 created and cancelled, 16 parallel | 0 failures and no `disk I/O error` |
| A write that stalls for 8 seconds and for 70 seconds | Succeeds |
| 3 GiB POST and GET | Contents match, 100 MiB |
| 1000 transfers in flight from the same Docker network, on `compose.yaml` with its read-only root and limits | 0 failures, 442 MiB at most, no OOM kill, 10 MiB afterwards |
| A 128 MiB data directory, `tmpfs` | 300 MiB upload and a 617 KB gzip body expanding to 300 MiB each answer 503 with guidance; the server stays ready and serves reads, and an upload succeeds after a delete |

Through Docker Desktop's published port, a thousand clients connecting at once got `connection reset by peer` from its port forwarding. The run from the same Docker network had no failures, so this scenario is recorded from there.

Before the image changed, 0.35% to 1.4% of session creations in the container failed with 503 `disk I/O error`, extended code 6410 (`SQLITE_IOERR_GETTEMPPATH`). An insert with a trigger needs a statement journal, which spills to a temporary file past 64 KiB, and `/tmp` in the image was not writable by its user. 0.11.0 had the same `/tmp`, and 5000 create and cancel cycles did not reach it, because its inserts had no trigger on this path. `scripts/smoke-containers.sh` now checks this.
