# Seed replay compatibility change

This is a breaking change for the next minor release. Seeding now follows
fake-gcs-server's `-data` behavior: a configured seed directory is applied on
every startup, not only when a data directory is first initialized.

## Changes

- Every startup applies the seed tree before readiness. Missing buckets and
  objects are created. An object whose bytes or metadata differ from what
  seeding writes, including API metadata edits, is replaced as a new
  generation. Only an object that exactly matches keeps its generation.
  Objects without a seed file are kept, and removing a seed file never deletes
  an object.
- An object under an event-based hold, temporary hold or unexpired retention
  that differs from its file is left unchanged, counted as `blocked`, and
  reported by a `seed_blocked` warning.
- Regular files directly under the seed root are skipped with a `seed_skipped`
  warning instead of stopping startup, as fake-gcs-server ignores them.
- The ready event's `seed` field reports `buckets`, `objects`, `replaced`,
  `unchanged`, `blocked` and `skipped_os_metadata`. The `existing_objects`
  field and the `skipped` outcome are removed.
- The default `seed_content_type` is now `extension`, so `logo.png` is stored
  as `image/png`. The same setting governs files ingested from `sync_dir`.
- The new `default_seed_dir` setting (`--default-seed-dir`,
  `GCS_MIMIC_DEFAULT_SEED_DIR`, JSON `default_seed_dir`) names a seed root used
  only when `seed_dir` is unset, the directory exists, and it does not overlap
  `data_dir` or `sync_dir`. The Docker image sets it to `/data`, so mounting
  fixtures at `/data` needs no other variable. The previous warning about a
  nonempty, unimported `/data` is removed.

## Compatibility and migration

- Deployments that set `seed_dir` and then change seeded objects through the
  API will see those changes replaced on the next restart. Remove `seed_dir`
  after the first start, or move fixtures that should stay editable out of the
  seed tree.
- Objects seeded as `application/octet-stream` by earlier releases are
  replaced once with the extension-derived type. Set
  `GCS_MIMIC_SEED_CONTENT_TYPE=octet-stream` to keep the previous type.
- An image deployment that mounts something other than fixtures at `/data`,
  without using it as `data_dir` or `sync_dir`, now seeds from it. Mount it
  elsewhere, or set `GCS_MIMIC_SEED_DIR` to the intended fixture directory.
- No catalog schema migration is introduced.

There is still no file watcher and no reseed endpoint; restart to apply edits.
See [loading local files](../container-usage.md#load-local-files) and the
[seed contract](../slices/seed.md).
