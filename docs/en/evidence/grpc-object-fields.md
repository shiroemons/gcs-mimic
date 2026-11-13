# gRPC v2 object fields: Google observation and local result

<!-- markdownlint-configure-file {"MD013": false} -->

Observed on 2026-10-31 against `storage.googleapis.com:443` (`google.storage.v2`, OAuth user credentials) with a scratch Go client built from the pinned Go SDK's generated `storagepb`. Project, bucket and principal names are omitted. The client differences were found by SDK probes with the official Java SDK 2.76.0 and Rust SDK 1.21.0 against gcs-mimic.

## Read masks

| Request | Google | gcs-mimic before | gcs-mimic now |
| --- | --- | --- | --- |
| GetObject, read_mask listing all 33 top-level `Object` fields (the mask the Java SDK sends) | OK; ACL entries, `owner` and `finalize_time` present | UNIMPLEMENTED for `acl`, `etag`, `owner`, `kms_key` and other fields | OK; ACL, owner, etag and storage-class time present; `kms_key`, `contexts` and `restore_token` empty because no local object has them |
| GetObject, read_mask `*` | OK, same content | OK without ACL/owner | OK, same as the full mask |
| GetObject, read_mask `[name, acl]` | OK; ACL present, `owner` and `finalize_time` absent | UNIMPLEMENTED | OK; ACL present, other fields empty |
| ListObjects, all-field read_mask | OK | UNIMPLEMENTED | OK |

The local ACL and owner come from the shared projection that renders JSON `projection=full`, including its omission under uniform bucket-level access and for callers without `storage.objects.getIamPolicy` (the read still succeeds). Test: `grpc_io_full_read_masks_return_acl_and_owner`.

## `finalize_time`

Google reports `finalize_time` on an ordinary finalized object (appendable objects were not observed on Google). The official Java SDK's appendable upload treats the upload as complete only when the returned resource carries `finalize_time`; without it, `finalizeAndClose` timed out after about 10 seconds against gcs-mimic.

gcs-mimic now reports `finalize_time` on every finalized generation, including a finalized appendable object, and omits it on unfinalized appendable generations. The value is the stored time that JSON reports as `timeFinalized` (the generation's creation time). Tests: `append_creates_flushes_reads_and_finalizes_with_reserved_generation`, `grpc_io_full_read_masks_return_acl_and_owner`.

## Checksums on a single-message appendable upload

The pinned proto's `BidiWriteObjectRequest.object_checksums` documentation allows appendable uploads to send checksums on the last request, the one with `finish_write`. The official Java SDK sends `write_object_spec`, data, `finish_write` and `object_checksums` in one message for a small appendable object; gcs-mimic rejected that message with INVALID_ARGUMENT because it was also the first. gcs-mimic now accepts it and verifies the CRC32C at finalization (a mismatch returns INVALID_ARGUMENT and leaves the object unfinalized). MD5 is still rejected, and first-message checksums without `finish_write` are still rejected before the object is created. Test: `append_accepts_checksums_on_a_single_finishing_message`.

## Checksum sub-paths in read masks

The official Java SDK's `ListObjects` mask names `checksums.md5_hash`; gcs-mimic rejected every nested path as UNIMPLEMENTED, which failed the SDK's basic object workflow. By protobuf field-mask semantics a sub-path selects only that child, so gcs-mimic now accepts `checksums.crc32c` and `checksums.md5_hash` and returns `checksums` with only the named digest. Other nested paths (for example `owner.entity`) are still UNIMPLEMENTED. This is derived from the field-mask contract and the SDK's request, not from a fresh Google observation. Test: `grpc_io_full_read_masks_return_acl_and_owner`. A later recording on 2026-11-13 confirmed the empty `checksums` message for GetObject and ListObjects on a composite and showed that ReadObject rejects a mask without `checksummed_data.content`, which gcs-mimic now enforces; see the [provider probes](provider-differential-gap-probes.md).

## Bucket `project`

`google.storage.v2.Bucket.project` is `projects/{project number}` on Google, and the Java SDK parses the value as a number (`StorageException: Illegal embedded sign character` for `projects/local-project`). gcs-mimic now renders the same project number as JSON `projectNumber` (a numeric project ID verbatim, `0` otherwise). Test: `grpc_record_conversion`.

## GetObject without a read mask

The pinned `storage.proto` documents the default `GetObject` mask as every field except `acl` and `owner`, and gcs-mimic follows it. The Rust scorecard probe for `object.acl` read the ACL without a mask and reported `ACL missing`; the probe now requests `acl` explicitly.
