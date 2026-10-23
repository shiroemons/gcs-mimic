# Object compose verification

<!-- markdownlint-configure-file {"MD013": false} -->

Checked on 2026-10-23 against the [compose contract](../slices/object-compose.md).
The feature ledger remains the implementation-status authority. The initial
compose implementation is commit `0488117b7b9526a8eb0ce70b87e986bc2c240b78`;
the shared source-deletion reclamation fix is
`daf43a03b0cee8a69dab2450142ca14d54efa0e9`. The final shared foundation is
`215e9148764493365ab4ec027b1ee8f8d2c488a8`; compose publication-boundary fixes are
`0ceae24905d80df899fe072474109645ebdc8b00`. The final source-delete permission and
noncurrent self-source acceptance cases are
`0feb5c555fa983c455c8c7e6de03a9abd7fcd1de`.

## Executed checks

| Command | Result and scope |
| --- | --- |
| `cargo fmt --all --check` | PASS |
| `cargo clippy --workspace --all-targets --locked --offline -- -D warnings` | PASS after publication-boundary fixes and the final shared foundation |
| `cargo test --workspace --locked --offline` | PASS after the final shared foundation and acceptance additions, including 6 body-validation, 18 wire, 2 service and 3 CRC32C property cases; [native log](object-compose-checks.txt) |
| `cargo test --locked --offline --test object_compose_http` | PASS, 18 wire cases |
| `GCS_MIMIC_GO_TEST_RUN='^TestObjectCompose' bash scripts/sdk-go.sh` | PASS, 2 pinned official SDK cases and go vet; [SDK log](object-compose-sdk-go.txt) |
| `cargo xtask dependencies check` | PASS at slice start; all 31 compiler, crate, SDK, CI and container inputs CURRENT; no dependency added |

Final-source full SDK, container and complete guide execution are pending the
coordinator's final-source signal. These have not been run for this slice yet.
Later final-source results replace this pending statement in this same evidence
set; no separate per-iteration evidence is maintained.

## Behavioral evidence

| Scope | Executable evidence and observations |
| --- | --- |
| Ordered construction | `compose_one_empty_repeated_and_nested_sources_exceed_old_component_limit` covers one empty component, repeated sources, 32 sources per call and nested counts above 1024; exact downloaded bytes match |
| Input boundary | All 6 `compose_body_*` validation cases; `compose_rejects_invalid_or_unsupported_input_without_mutation` and `compose_method_body_and_selector_boundaries` cover wire types, caps, unsupported settings, fields and method errors |
| Generation and atomic publication | `compose_source_and_destination_preconditions_preserve_failed_overwrites` covers selected noncurrent source and source/destination failures; `compose_concurrent_create_if_absent_has_one_winner` observes one success and seven 412 responses |
| Source pinning and restart | Shared `pinned_ranges_resume_after_source_deletion_restart_and_uncommitted_tail`, `batch_source_resolution_checks_every_source_and_cleans_partial_pins` and `recovery_restores_renamed_uncommitted_output_and_empty_sources` exercise pinned bytes, one snapshot and rename recovery |
| Composite metadata | `compose_order_checksums_metadata_fields_and_restart` verifies metadata/list/fields, CRC32C, absent MD5, generation/metageneration, custom metadata, metadata updates and exact restart equality |
| CRC32C and component count | `every_split_matches_direct_crc32c`, `arbitrary_ordered_components_match_direct_crc32c`, `nested_combination_is_associative_and_preserves_empty_components`, `component_count_saturates_and_success_cleans_temporary_links`; no payload reread is needed to combine source CRC32Cs |
| XML metadata | The metadata/restart wire case verifies GET/HEAD component-count headers only on composites, JSON/XML composite ETag equality, CRC32C-only hash headers and ETag change after metadata update; the opaque spelling is explicitly local |
| Classes, holds and versioning | `compose_storage_class_defaults_and_event_holds` covers same/mixed classes, source-class inheritance, source read holds, destination defaults/explicit holds and held overwrite denial; source/destination condition and deletion cases cover retained generations |
| Strict permissions | `compose_strict_requires_both_read_create_and_delete_for_destructive_cases` verifies allowed combined grants and denied read-only/create-only/anonymous/overwrite/deletion requests |
| Publication permission changes | `compose_rechecks_read_and_create_grants_after_streaming` revokes grants at the durable checkpoint with a temporary SQLite trigger, proving final read/create checks without a timing race; no destination is published |
| Source delete permission and self-source holds | `compose_rechecks_source_delete_grant_while_read_and_create_remain_allowed` removes only delete permission and requires 403 with the source retained; `compose_same_name_noncurrent_source_hold_is_rechecked_before_publication` holds a selected noncurrent self-source while leaving the live destination unheld, requiring rejection by the selected-source validation loop |
| Publication state changes | Checkpoint injection in `compose_hold_added_after_streaming_preserves_destination_and_sources`, `compose_source_generation_change_never_deletes_the_replacement` and `compose_destination_condition_is_rechecked_after_streaming` proves held/disappeared source generation and destination-condition failures after initial validation, preserving a replacement source and the old destination |
| Hard deletion | `compose_hard_deletion_is_atomic_deduplicated_and_preserves_new_destination` verifies permanent selected-source deletion under versioning, duplicate deletion once, normal destination history, retained unrelated source history, 3 remaining physical blobs and zero pending deletions before restart |
| Late failure rollback | `compose_late_source_delete_failure_rolls_back_destination_and_earlier_deletions` injects an abort on the second source DELETE, after destination insertion and the first source deletion; the entire prior resources/bytes remain and staging/journal are empty |
| Object cap and cleanup | `composed_size_cap_preserves_sources_and_leaves_no_transfer` rejects an oversized aggregate before publication with all sources intact and no staging/transfer record |
| Ephemeral recovery | Shared `restart_reclaims_ephemeral_transfers_and_preserves_continuable_transfers`, `abandoned_handler_guard_waits_for_writer_then_reclaims_transfer` and injected-clock maintenance tests cover request-owned staging without claiming power-loss durability |
| Cleanup failure and incarnation | `compose_incarnation_change_preserves_original_error_and_recovers_after_restart` changes the bucket incarnation at checkpoint, rejects with local 412, injects cleanup failure, preserves that original response and recovers the retained ephemeral transfer on restart |
| Official SDK | `TestObjectCompose` covers ordered/generation-pinned/conditioned sources, destination metadata/conditions, ignored JSON destination CRC32C, self-source append and source deletion; `TestObjectComposeStrictPermissions` covers allowed admin and denied read-only/create-only callers |

Go inputs are toolchain 1.27.3, cloud.google.com/go/storage v1.70.0 and
google.golang.org/api v0.308.0, from the committed module graph. The harness uses
guarded explicit loopback endpoints in permissive and strict profiles and blocks
cloud/metadata-server access.

## Scope boundaries

The current official quota replaces the obsolete 1024 component limit with
unlimited cumulative components and int32-saturating metadata. JSON exposes
deleteSourceObjects in both current Discovery and method documentation.
Source SHA-256 values and proto commit pins are in the slice contract.

The documented core construction, count/checksum metadata and hard-delete flag
are separate from local deletion safety rules: additional delete permission,
held-source rejection, duplicate handling, destination-name exclusion and one
publication/deletion transaction. Provider questions about these boundaries,
JSON checksum wording, MIME/null defaults, unknown/output-only fields, error
precedence, JSON component-count header presence and opaque ETag format remain
`SPEC-COMPOSE-EDGES` outside the declared local acceptance scope. Every documented
compose request field is implemented or explicitly rejected with a documented
error; advanced ACL, encryption/KMS, billing, contexts, temporary holds, retention
and explicit storageClass inputs return 501. All four rows retain a pending final
integration gate and have not been promoted yet. XML compose and Storage v2
compose remain outside this slice.

No real-cloud requests, process/cgroup memory or CPU benchmark, physical Google
durability/billing, host power loss or exhaustive per-instruction fsync testing
was performed. Bounded 64 KiB streaming, short catalog transactions, opaque blobs,
admission and the existing recovery journal are implementation/contract evidence,
not comparative performance measurements.

A compose transfer has a fixed 15-minute ephemeral lifetime. Increasing the HTTP
object-request timeout does not extend the publication deadline; expiry returns
local 410. This resource limit is separate from the 1 GiB default object cap and
does not claim maximum-size throughput or precise interruption of stalled I/O.
