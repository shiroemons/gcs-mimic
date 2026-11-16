# Bucket purge

<!-- markdownlint-configure-file {"MD013": false} -->

Scope: the bucket purge subset of DESIGN-068. It is a local management route for test helpers that clean up the bucket a test created, such as a per-test temporary bucket. It is not a Google API operation, and `DELETE /storage/v1/b/{bucket}` keeps refusing a bucket with contents or protections. The compatibility ledger retains the broader incomplete cases.

## Request and authority

Send `DELETE /_internal/buckets/{bucket}` with an empty body and no query parameters. The route takes the management guards in the order every local management route takes them ([management request boundaries](http-adapter-boundaries.md)): a browser-initiated request returns 403 and an unapproved public DNS Host returns 421, then disabled management returns 404, then strict mode requires the bearer principal that `--admin-principal` names (401 without a credential, 403 for any other principal, whatever its Google IAM roles).

| Request | Answer |
| --- | --- |
| Another method | 405 `methodNotAllowed` with `Allow: DELETE` |
| Any query parameter | 400 `invalidArgument` |
| A non-empty body | 400 `invalidArgument` |
| A name that is not a bucket name | 400 `invalidParameter`, `Invalid bucket name` |
| A name with no live or soft-deleted bucket | 404 `notFound` |
| An import is incomplete | 409 `conflict`, no changes |
| Directory synchronization maps the bucket | 409 `conflict`, no changes; external files are never touched |
| A request writes one of the bucket's sessions or appendable objects | 503 `backendError` with `Retry-After: 1`, no changes |
| The catalog wait expires before the purge gets the writer connection | 503 `backendError` with `Retry-After: 1`, no changes |
| A failure the purge cannot resolve | 503 `backendError` with `Retry-After: 1`, "could not confirm completion" |

The path segment is decoded as the JSON API decodes a bucket segment (percent-decoding, `+` kept, at most three times the longest name in encoded bytes), then held to the native bucket-name rules that every local administration route uses. A bucket whose name only the `fake-gcs-server` profile admits cannot be purged; delete it through the JSON API. A virtual-hosted request whose path looks like the route names an XML object, as with the other management paths.

A successful purge answers 200 with the number of object rows it removed and whether cleanup after commit was deferred:

```json
{"objects":2,"cleanupPending":false}
```

`objects` counts every generation: live, noncurrent and soft-deleted generations of the live bucket and the retained objects of its soft-deleted versions. Test helpers treat 404 as an already clean bucket.

## What the transaction removes

One immediate transaction on the writer connection removes everything the catalog stores under the bucket's name. Holds, object retention and bucket retention policies are not checked, as reset does not check them, and the bucket is not moved to soft-deleted: no version of it remains to restore.

| State | How it goes |
| --- | --- |
| Objects of the live bucket and of each soft-deleted version | Deleted; each blob joins the deletion journal |
| Soft-deleted versions of the name | Deleted |
| The bucket row, IAM policy and incarnation | Deleted, through the helper `Catalog::delete_bucket` shares |
| Bucket and object ACLs, notification configurations, the initialization record, XML multipart parts | Foreign-key cascades |
| Resumable sessions | Deleted; blobs nothing references join the deletion journal |
| XML multipart uploads | Retired; parts join the multipart deletion journal |
| Appendable objects | Deleted; RAM data joins the deletion journal, disk data files are removed after commit |
| Rewrite sessions that read from or write to the bucket | Deleted; their derived transfers are discarded after commit |
| Upload replay records of the current incarnation | Deleted |
| Queued notification events that name the bucket | Deleted from the outbox |
| Directory synchronization object rows | Deleted |
| The change journal | Receives one bucket deletion record while the bucket's identity still exists |

`purge_classifies_every_catalog_table` classifies every catalog table as deleted, cascaded, bound to an incarnation or instance-wide, so a later table must be classified before the test passes, and every deleted or cascaded table must have a check in the purge tests that its rows of the bucket are gone.

Objects are deleted a page at a time before their blobs are journaled, so the journal reuses the pages the rows free. A RAM catalog at its page budget can therefore still be emptied, which journaling every blob first would refuse; `purge_empties_a_ram_catalog_at_its_page_budget` covers it.

The commit goes through the fault-seamed publication commit. A commit can succeed before it reports an error; the purge then reads whether any version of the name is left, and a name left with none means the commit succeeded.

## Exclusion

The purge takes no instance-wide gate. It waits up to `catalog_wait_timeout_ms` and the request deadline for its turn in the administrative admission and execution queues, like any other management call, and then holds only the writer connection and the writers of the bucket's own sessions. Uploads and reads of other buckets keep going: their catalog jobs queue for the writer connection only as long as the purge's short transaction takes, and an upload in flight does not hold the purge back. A test's cleanup therefore does not stall tests that run alongside it.

Inside the transaction the purge claims the writer of every resumable session, XML multipart upload and rewrite session of the bucket, and checks that no in-process user holds an appendable object of it. Any held writer refuses the purge with 503 before anything changes: a request is writing, and waiting for it would hold the writer connection meanwhile. The claims last until the files of the deleted rows are gone, so no request writes one of those sessions in between. A new session, and a new or taken-over appendable writer, can only start through the writer connection, which the purge holds.

A simple upload to the bucket that is already streaming its body holds no session; it fails at publication once the bucket is gone. As with an ordinary bucket deletion, an upload accepted before the purge could publish into a bucket recreated under the same name before it finishes; per-test bucket names avoid this.

## Files, failure and recovery

After commit, still holding the writers, the purge removes the staging files of deleted resumable sessions and the data files of deleted appendable objects (disk only; a RAM catalog journals those bytes), discards the derived transfers of deleted rewrite sessions, drains the multipart deletion journal and reclaims the deletion journal through the reference-checked reclamation that maintenance uses. It never uses the RAM backend's clear-everything path, which would also drop the bodies of other buckets' uploads in flight.

A failure in any of these steps cannot undo the purge. It logs `bucket_purge_cleanup_deferred`, wakes maintenance and answers `cleanupPending: true`. The journals stay for maintenance and startup recovery; staging and appendable files that no journal names are removed by the next open, whose recovery removes every staging and appendable entry no row owns. A failure before commit changes nothing for the bucket or any other.

Reclamation drains the whole deletion journal, not only the purge's entries, and fails when any journaled blob is still referenced. A cleanup left over from earlier work can therefore make `cleanupPending` true for an unrelated reason; helpers should not treat it as a failed purge.

## Acceptance and verification

Storage tests in `src/storage/catalog/admin/purge/tests.rs`:

- `purge_classifies_every_catalog_table`
- `purge_removes_every_generation_session_and_record_past_holds_and_spares_other_buckets`: a soft-deleted version with a retained object, then a live versioned bucket with current, noncurrent and soft-deleted generations under both holds, locked object retention and a locked bucket retention policy; one resumable session, XML multipart upload with a part, appendable object and rewrite in progress; a notification configuration with a queued event, a bucket ACL, an initialization record, a sync row and a replay record. Every such row, every derived transfer and both journals are gone afterwards; the other bucket keeps its objects, session staging and queued event; only its files remain on disk; restart and recreating the name start empty.
- `purge_on_memory_returns_the_buckets_ram_and_keeps_other_uploads` and `purge_empties_a_ram_catalog_at_its_page_budget` for the RAM backend.
- `purge_does_not_wait_for_an_upload_to_another_bucket`, on disk and in RAM: an upload to another bucket holds its lease through the purge and then finishes.
- `purge_refuses_without_changes_a_missing_name_an_import_synchronization_and_writers`: missing name, incomplete import, a synchronization mapping, a held resumable writer and a live appendable writer, then a purge that succeeds once they are gone.
- `purge_failed_commit_changes_nothing_and_an_error_after_commit_resolves_as_success`
- `failed_purge_cleanup_answers_pending_and_maintenance_or_restart_finishes_it`: a failed blob unlink, which maintenance then reclaims, and a failed staging-directory sync, which the next open finishes.
- `purge_crash_and_power_loss_snapshots_recover_the_bucket_whole_or_not_at_all`: process-abort and Unix power-loss snapshots before and after commit, at the session-file unlink sync and after it, and at reclamation's unlink, shard sync and journal removal. Each reopens with the bucket's rows all present (before commit) or all gone, the other bucket's object and session intact, and no leftover staging or blob file.

Wire tests in `tests/it_admin/admin_http/admin_purge/mod.rs`: `bucket_purge_is_opt_in_and_refuses_malformed_requests_without_changes`, `bucket_purge_deletes_every_generation_and_session_past_holds_and_keeps_other_buckets` (disk and memory backends), `bucket_purge_answers_503_while_a_session_of_the_bucket_writes_and_never_waits_for_others`, `bucket_purge_uses_the_management_guards_and_the_local_manager`, `virtual_host_purge_lookalike_keeps_object_semantics` and, on Linux and macOS, `bucket_purge_refuses_a_bucket_directory_synchronization_maps`. `server::admin::purge::tests` pins the path decoding and every failure's status, reason and message. The guard and page-initiated request tests list the route beside the other management routes.

Verification on 2026-11-16 (macOS): `cargo test --locked --lib -- storage::catalog::admin::purge server::admin::purge` passed 11 cases and `cargo test --locked --test it_admin admin_purge` passed 6. `cargo test --workspace --locked` passed 3,779 tests with 1 existing ignored test and no failures. `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings` and `cargo xtask lint` passed with zero new production violations and zero ratcheted findings. `cargo xtask compatibility render` left both feature matrices unchanged, and `cargo xtask compatibility check` passed.

The integrated source was then verified on 2026-11-16: `bash scripts/check-cross.sh` passed the Windows MSVC, Linux GNU and musl type checks, and the Foundation checks workflow passed on GitHub runners, running `cargo test --workspace` natively on Linux (pinned and current stable), macOS, Windows x64 and Linux musl, the container smoke on linux/amd64 and linux/arm64, the Go SDK job and Testcontainers. Every pinned SDK suite passed locally against isolated servers. The route has no Google counterpart, so no provider differential test applies.
