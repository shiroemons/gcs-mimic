# Stored lifecycle Delete rules now execute

This change targets the next release. Previously, lifecycle rules were stored and returned without executing their actions. Stored `Delete` rules now execute automatically after upgrade, including rules already present in the catalog. Existing objects that satisfy those rules can therefore be deleted immediately after maintenance starts.

To preserve store-only behavior, start with `--disable-lifecycle true`, `GCS_MIMIC_DISABLE_LIFECYCLE=true`, or JSON configuration `"disable_lifecycle": true`. The default is `false`, with normal CLI > environment > JSON > default precedence. The startup ready event reports the effective value.

Deletion respects holds, bucket retention, object retention, versioning and each bucket's soft-delete policy. Versioned live objects first become noncurrent; matching noncurrent generations are removed on later passes. Soft-deleted generations remain under their existing expiry policy. Buckets without soft delete permanently lose deleted bytes.

The guarded `POST /_internal/lifecycle/run` route runs one pass using the current clock and returns action counts. `SetStorageClass` and `AbortIncompleteMultipartUpload` remain stored without execution in this slice. No catalog migration is required. See the [execution contract](../slices/lifecycle-execution.md) for scheduling, supported scope and provider differences.
