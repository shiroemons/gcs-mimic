# Bucket soft delete and restoration evidence

<!-- markdownlint-configure-file {"MD013": {"code_blocks": false}} -->

Contract and acceptance cases: [bucket-soft-delete-restore.md](../slices/bucket-soft-delete-restore.md).

## Cloud Storage observation

One opted-in run against Cloud Storage on 2026-10-31, with a bucket whose soft
delete policy was seven days. Project, bucket and principal names are omitted.

- A live bucket resource includes `generation`.
- Deleting the bucket after deleting its only object returned 204; a plain GET
  then returned 404.
- `GET /b/{bucket}?softDeleted=true` without `generation` returned
  400 "Bucket generation is required."
- `GET /b?project=..&softDeleted=true&prefix=..` returned 200 with the bucket,
  the same `generation` and a `softDeleteTime`.
- `POST /b/{bucket}/restore?generation=G` returned 200 with the bucket resource,
  the same `generation` and no `softDeleteTime`.
- After the restore, the object deleted before the bucket was still soft-deleted
  and listed with `softDeleted=true`; it was not restored with the bucket.
- `GET /b/{bucket}?softDeleted=true&generation=G` returned 403 naming
  `storage.buckets.get` for the project owner. This is inconclusive (possibly
  permission propagation), so the local implementation follows the documented
  `storage.buckets.get` requirement and returns the version to authorized
  callers. This point stays open under SPEC-BUCKET-SOFT-DELETE.

The local implementation matches every conclusive point above: the generation on
live and soft-deleted resources, 404 after deletion, the exact 400 message, listing
with the same generation, restore with the same generation and no deletion times,
and objects that stay soft-deleted after a bucket restore.

## Verification

Run on 2026-10-31 with the pinned Rust toolchain and unchanged dependencies.

- Before the implementation, the four new `tests/bucket_soft_delete_http.rs`
  tests failed because the bucket resource had no `generation`.
- `cargo test --workspace --locked --offline --no-fail-fast -- --test-threads=4`:
  PASS, 68 test targets and 1819 tests with no failures.
  `rewrite_initial_catalog_wait_and_streaming_finish_before_returning_success` in
  `tests/rewrite_http.rs` covers the maintenance sweep: it holds the catalog
  write lock while a rewrite streams. The sweep must not take the write lock
  when no soft-deleted bucket is due, or it blocks the catalog connection.
- `cargo clippy --all-targets --locked -- -D warnings`: PASS.
- `cargo xtask lint`: PASS, 0 production violations.
- `cargo xtask compatibility check`: PASS.
- The .NET probe below ran against a local `gcs-mimic serve --scheme http`
  built from this source, with Google.Cloud.Storage.V1 5.0.0 on net10.0: PASS.

```text
$ SCORECARD_ENDPOINT=http://127.0.0.1:<port> SCORECARD_SIGNING_KEY=fixtures/signing/test-only-private.pem SCORECARD_RUN=bucket_soft_delete_restore bash sdk-scorecard/dotnet/run.sh --no-build
{"sdk":"dotnet","id":"sdk.bucket_soft_delete_restore","area":"sdk","pass":true}
```
