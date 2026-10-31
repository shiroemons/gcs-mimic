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
