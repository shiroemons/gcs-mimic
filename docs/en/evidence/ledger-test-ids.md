# Ledger test IDs reconciled with the code

<!-- markdownlint-configure-file {"MD013": false} -->

Verified on **2026-11-07**. The feature ledger named 36 test IDs that no longer existed in the code: tests had been renamed, merged into tables or rewritten when behavior changed, without a ledger update. This records what each ID became, so the ledger's verification lists can drop a name no current code answers to. `git log -S<id>` finds when each one went away.

`cargo xtask compatibility` now fails when an identifier-shaped test ID names nothing in the code; see [compatibility policy](../compatibility-policy.md).

## Replaced where the ledger already named the replacement

The ledger already recorded a run of the replacement for these, so only the old name went away.

| Old ID | Current test | What changed |
| --- | --- | --- |
| `basic_contract_media_hash_matrix_validates_each_checksum_and_preserves_failed_overwrites` | `basic_contract_media_hash_matrix_stores_computed_hashes_and_ignores_declared_ones` | A JSON media upload ignores a declared hash, as recorded on Cloud Storage. |
| `bucket_get_rejects_patch_only_acl_query_parameters` | `bucket_get_ignores_or_rejects_patch_only_acl_query_parameters` | Rejection stays only in the legacy profile. |
| `bucket_patch_accepts_only_empty_lifecycle_rules` | `bucket_patch_lifecycle_invalid_input_preserves_state` | Nonempty lifecycle rules are stored and invalid ones refused. |
| `bucket_patch_rejects_get_only_generation_query_parameter` | `bucket_patch_get_only_generation_query_parameter_follows_profile` | Rejection stays only in the legacy profile. |
| `bucket_put_omitted_settings_survive_partial_iam_updates` | `bucket_put_keeps_the_class_soft_delete_and_access_settings_it_omits` | A native PUT replaces the resource. |
| `bucket_put_rejects_get_only_generation_query_parameter` | `bucket_put_get_only_generation_query_parameter_follows_profile` | Rejection stays only in the legacy profile. |
| `bucket_put_ruby_update_block_preserves_unrelated_settings` | `bucket_put_ruby_update_block_replaces_and_setters_patch` | The Ruby update block's PUT replaces omitted settings. |
| `storage::catalog::objects::retention::tests::column_keeps_nanoseconds_and_reads_microsecond_rows_unchanged` | `storage::catalog::objects::retention::tests::read_restores_the_exact_nanosecond_from_the_columns` | The remainder moved to its own column. |
| `bucket_http::satisfies_pzi_is_true_in_every_location` | `bucket_http::satisfies_pzi_marks_regional_buckets_and_rpo_marks_the_others` | Only regional buckets carry `satisfiesPZI`. |
| `held_object_is_blocked_while_identical_held_object_is_unchanged` | `held_seed_changes_roll_back_the_entire_tree_while_identical_held_objects_are_unchanged` | A differing held fixture fails the whole replay. |
| `media_upload_hash_is_checked_against_the_stored_bytes` | `media_upload_stores_computed_hashes_whatever_the_declared_md5` | A JSON media upload ignores a declared hash. |
| `json::object_tests::obj_headers_upload_tokens_require_generation_match_and_one_nonempty_value` | `json::object_tests::obj_headers_upload_tokens_require_one_nonempty_value_with_or_without_conditions` | Tokens no longer need a generation precondition. |
| `obj_http_idempotency_token_local_read_delete_rule_and_upload_rejection` | `obj_http_idempotency_token_local_read_delete_rule_and_upload_replay` | An unconditional token replays instead of failing. |
| `obj_http_upload_tokens_reject_unsafe_or_invalid_headers_before_body` | `obj_http_upload_tokens_reject_empty_or_duplicate_headers_before_body` | The unconditional token rejection went away. |
| `obj_multipart_http_upload_tokens_reject_empty_or_duplicate_headers_before_body` | `obj_http_upload_tokens_reject_empty_or_duplicate_headers_before_body` | Merged into a table, as its multipart scenario. |
| `obj_multipart_http_upload_tokens_reject_unsafe_or_invalid_headers_before_body` | `obj_http_upload_tokens_reject_empty_or_duplicate_headers_before_body` | Merged into a table; the unconditional rejection went away. |
| `obj_multipart_http_upload_tokens_with_generation_match_allow_one_create_and_overwrite` | `obj_http_upload_tokens_with_generation_match_allow_one_create_and_overwrite` | Merged into a table, as its multipart scenarios. |

## Replaced with a new verification record

The ledger had no run of these replacements, so their rows gained a record of the run below.

| Old ID | Current test | What changed |
| --- | --- | --- |
| `bkt_id_002_etag_changes_with_revision_and_incarnation` | `bkt_id_002_etag_encodes_only_the_metageneration` | ETags follow the recorded encoding, which carries no incarnation. |
| `config_input::tests::data_inspection_is_bounded_conservative_and_resolves_missing_ancestors` | `config_input::tests::default_seed_is_adopted_only_when_present_and_disjoint` | An existing disjoint `/data` is adopted as the seed instead of warned about. |
| `go_sdk_scope_and_lockfile_are_declared` | `sdk_scopes_and_lockfiles_are_declared` | Renamed when the table grew past Go. |
| `ingest_strict_configuration_has_actionable_remedy` | `ingest_strict_configuration_requires_an_identity_registry` | Strict synchronization is accepted, given a registry. |
| `bucket_init::tests::initializer_location_uses_default_only_when_omitted` | `bucket_init::tests::initializer_location_normalizes_names_and_defaults` | Regional names are normalized and an empty value means the default. |
| `listener_failure_drains_background_work_before_catalog_close` | `fatal_listener_failure_drains_background_work_before_catalog_close` | Renamed; temporary accept failures keep accepting. |
| `retention_roundtrip_http::locked_retention_read_back_is_accepted_and_a_shorter_time_is_not` | `retention_roundtrip_http::locked_retention_refuses_every_reduction_with_403_and_accepts_equal_or_later` | A Locked reduction is 403 `forbidden`, as recorded. |
| `metadata_updates_accept_only_echoed_acls` | `metadata_updates_replace_and_clear_acls` | Metadata updates replace and clear ACLs. |
| `poisoned_catalog_permanently_revokes_readiness` | `storage::catalog::tests::panicked_operation_rolls_back_and_keeps_the_catalog_ready` | A panic rolls back; only an unconfirmed rollback revokes readiness. |
| `stalled_uploads_do_not_occupy_blocking_workers_and_admission_is_bounded` | `storage::object_tests::stalled_uploads_leave_blocking_workers_available_without_an_upload_limit` | Uploads no longer have their own admission limit. |
| `v1_migration_is_transactional_and_v3_reopens_idempotently` | `v1_migration_is_transactional_and_current_schema_reopens_idempotently` | Renamed; v1 migrates to the current schema. |

## Lanes spelled as labels

These never were test functions. Each names a script or manual lane described by its row's `verification[].command`, so they now take the kebab-case of every other lane label.

| Old ID | Label |
| --- | --- |
| `differential_live_record` | `differential-live-record` |
| `differential_offline_replay` | `differential-offline-replay` |
| `differential_partial_merge` | `differential-partial-merge` |
| `differential_self_check` | `differential-self-check` |
| `go_https_sdk_suite` | `go-https-sdk-suite` |
| `ruby_https_sdk_journey` | `ruby-https-sdk-journey` |
| `tls_browser_trust` | `tls-browser-trust` |
| `tls_native_container_journeys` | `tls-native-container-journeys` |
