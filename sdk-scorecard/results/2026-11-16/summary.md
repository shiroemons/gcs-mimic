# SDK scorecard

<!-- markdownlint-configure-file {"MD013": false, "MD024": {"siblings_only": true}} -->

Each SDK contributes 100/9 (about 11.1) points; cases within an SDK have equal weight.
Cases the SDK has no public API for are shown as n/a and excluded from that SDK's denominator.
Score counts every case, including each SDK's own idioms (sdk area) and transports (grpc area).
Core counts only the bucket/object/signed cases shared by every SDK, for cross-SDK comparison.

| Emulator | Version | Go | Ruby | Node.js | PHP | Python | Java | .NET | Rust | C++ | Score (/100) | Core (/100) |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| fake-gcs-server | `fsouza/fake-gcs-server:1.56.1` | 28/66 (1 n/a) | 25/57 (1 n/a) | 28/59 | 29/60 | 23/63 | 36/79 (1 n/a) | 32/72 (1 n/a) | 0/54 (5 n/a) | 27/61 (1 n/a) | 39.2 | 38.5 |
| floci-gcp | `floci/floci-gcp:0.13.0` | 54/66 (1 n/a) | 45/57 (1 n/a) | 49/59 | 48/60 | 44/63 | 58/79 (1 n/a) | 53/72 (1 n/a) | 32/54 (5 n/a) | 50/61 (1 n/a) | 75.8 | 76.2 |
| gcp-storage-emulator | `oittaa/gcp-storage-emulator:v2026.07.19` | 21/66 (1 n/a) | 24/57 (1 n/a) | 25/59 | 22/60 | 25/63 | 35/79 (1 n/a) | 31/72 (1 n/a) | 0/54 (5 n/a) | 25/61 (1 n/a) | 35.7 | 36.3 |
| gcs-mimic | `gcs-mimic 0.25.0 (f97619d1)` | 66/66 (1 n/a) | 57/57 (1 n/a) | 59/59 | 60/60 | 63/63 | 79/79 (1 n/a) | 72/72 (1 n/a) | 54/54 (5 n/a) | 61/61 (1 n/a) | 100 | 100 |

## Passed cases by area

### Go

| Area | fake-gcs-server | floci-gcp | gcp-storage-emulator | gcs-mimic |
| --- | --- | --- | --- | --- |
| bucket | 5/20 | 18/20 | 7/20 | 20/20 |
| object | 16/29 | 24/29 | 10/29 | 29/29 |
| signed | 4/6 | 4/6 | 3/6 | 6/6 |
| grpc | 0/8 | 5/8 | 0/8 | 8/8 |
| sdk | 3/3 | 3/3 | 1/3 | 3/3 |

### Ruby

| Area | fake-gcs-server | floci-gcp | gcp-storage-emulator | gcs-mimic |
| --- | --- | --- | --- | --- |
| bucket | 5/21 | 16/21 | 8/21 | 21/21 |
| object | 14/28 | 23/28 | 11/28 | 28/28 |
| signed | 4/6 | 4/6 | 3/6 | 6/6 |
| sdk | 2/2 | 2/2 | 2/2 | 2/2 |

### Node.js

| Area | fake-gcs-server | floci-gcp | gcp-storage-emulator | gcs-mimic |
| --- | --- | --- | --- | --- |
| bucket | 5/21 | 18/21 | 8/21 | 21/21 |
| object | 16/29 | 24/29 | 11/29 | 29/29 |
| signed | 5/6 | 4/6 | 3/6 | 6/6 |
| sdk | 2/3 | 3/3 | 3/3 | 3/3 |

### PHP

| Area | fake-gcs-server | floci-gcp | gcp-storage-emulator | gcs-mimic |
| --- | --- | --- | --- | --- |
| bucket | 5/21 | 17/21 | 8/21 | 21/21 |
| object | 16/29 | 23/29 | 9/29 | 29/29 |
| signed | 5/6 | 4/6 | 3/6 | 6/6 |
| sdk | 3/4 | 4/4 | 2/4 | 4/4 |

### Python

| Area | fake-gcs-server | floci-gcp | gcp-storage-emulator | gcs-mimic |
| --- | --- | --- | --- | --- |
| bucket | 3/21 | 15/21 | 11/21 | 21/21 |
| object | 12/29 | 22/29 | 10/29 | 29/29 |
| signed | 5/6 | 4/6 | 3/6 | 6/6 |
| grpc | 0/2 | 0/2 | 0/2 | 2/2 |
| sdk | 3/5 | 3/5 | 1/5 | 5/5 |

### Java

| Area | fake-gcs-server | floci-gcp | gcp-storage-emulator | gcs-mimic |
| --- | --- | --- | --- | --- |
| bucket | 4/20 | 13/20 | 10/20 | 20/20 |
| object | 16/29 | 23/29 | 12/29 | 29/29 |
| signed | 5/6 | 4/6 | 3/6 | 6/6 |
| grpc | 0/10 | 7/10 | 0/10 | 10/10 |
| sdk | 11/14 | 11/14 | 10/14 | 14/14 |

### .NET

| Area | fake-gcs-server | floci-gcp | gcp-storage-emulator | gcs-mimic |
| --- | --- | --- | --- | --- |
| bucket | 4/21 | 16/21 | 10/21 | 21/21 |
| object | 14/28 | 21/28 | 12/28 | 28/28 |
| signed | 4/6 | 4/6 | 3/6 | 6/6 |
| sdk | 10/17 | 12/17 | 6/17 | 17/17 |

### Rust

| Area | fake-gcs-server | floci-gcp | gcp-storage-emulator | gcs-mimic |
| --- | --- | --- | --- | --- |
| bucket | 0/17 | 8/17 | 0/17 | 17/17 |
| object | 0/29 | 20/29 | 0/29 | 29/29 |
| signed | 0/5 | 3/5 | 0/5 | 5/5 |
| grpc | 0/1 | 0/1 | 0/1 | 1/1 |
| sdk | 0/2 | 1/2 | 0/2 | 2/2 |

### C++

| Area | fake-gcs-server | floci-gcp | gcp-storage-emulator | gcs-mimic |
| --- | --- | --- | --- | --- |
| bucket | 5/20 | 18/20 | 9/20 | 20/20 |
| object | 15/29 | 23/29 | 11/29 | 29/29 |
| signed | 5/6 | 4/6 | 3/6 | 6/6 |
| sdk | 2/6 | 5/6 | 2/6 | 6/6 |

## Case results

### Go

| Case | fake-gcs-server | floci-gcp | gcp-storage-emulator | gcs-mimic |
| --- | --- | --- | --- | --- |
| `bucket.create_get_delete` | ✅ | ✅ | ✅ | ✅ |
| `bucket.list_prefix_paging` | ✅ | ✅ | ❌ | ✅ |
| `bucket.update_labels` | ❌ | ✅ | ❌ | ✅ |
| `bucket.metageneration_precondition` | ❌ | ❌ | ❌ | ✅ |
| `bucket.versioning_history` | ✅ | ✅ | ❌ | ✅ |
| `bucket.lifecycle` | ❌ | ✅ | ✅ | ✅ |
| `bucket.cors` | ❌ | ✅ | ✅ | ✅ |
| `bucket.retention_policy` | ❌ | ✅ | ❌ | ✅ |
| `bucket.retention_lock` | ❌ | ✅ | ❌ | ✅ |
| `bucket.uniform_bucket_level_access` | ❌ | ✅ | ✅ | ✅ |
| `bucket.public_access_prevention` | ❌ | ✅ | ✅ | ✅ |
| `bucket.iam_policy` | ❌ | ✅ | ✅ | ✅ |
| `bucket.iam_test_permissions` | ❌ | ✅ | ✅ | ✅ |
| `bucket.soft_delete_policy` | ❌ | ✅ | ❌ | ✅ |
| `bucket.soft_delete_restore` | n/a | n/a | n/a | n/a |
| `bucket.autoclass` | ❌ | ❌ | ❌ | ✅ |
| `bucket.acl` | ✅ | ✅ | ❌ | ✅ |
| `bucket.default_object_acl` | ❌ | ✅ | ❌ | ✅ |
| `bucket.notifications` | ✅ | ✅ | ❌ | ✅ |
| `project.hmac_keys` | ❌ | ✅ | ❌ | ✅ |
| `project.service_account` | ❌ | ✅ | ❌ | ✅ |
| `object.write_read_small` | ✅ | ✅ | ✅ | ✅ |
| `object.resumable_large` | ✅ | ✅ | ❌ | ✅ |
| `object.metadata_on_write` | ✅ | ✅ | ✅ | ✅ |
| `object.checksum_rejected` | ✅ | ❌ | ✅ | ✅ |
| `object.md5_accepted` | ✅ | ✅ | ✅ | ✅ |
| `object.range_reads` | ✅ | ✅ | ❌ | ✅ |
| `object.update_metadata` | ❌ | ✅ | ❌ | ✅ |
| `object.delete_not_found` | ✅ | ✅ | ✅ | ✅ |
| `object.does_not_exist_precondition` | ✅ | ✅ | ❌ | ✅ |
| `object.generation_match_precondition` | ❌ | ✅ | ❌ | ✅ |
| `object.metageneration_match_precondition` | ❌ | ✅ | ❌ | ✅ |
| `object.list_prefix_delimiter` | ✅ | ✅ | ✅ | ✅ |
| `object.list_paging` | ✅ | ✅ | ❌ | ✅ |
| `object.list_offsets` | ✅ | ✅ | ❌ | ✅ |
| `object.list_match_glob` | ❌ | ✅ | ✅ | ✅ |
| `object.copy_with_metadata` | ✅ | ❌ | ❌ | ✅ |
| `object.rewrite_storage_class` | ❌ | ✅ | ❌ | ✅ |
| `object.compose` | ❌ | ✅ | ❌ | ✅ |
| `object.compose_preconditions` | ❌ | ❌ | ❌ | ✅ |
| `object.move` | ❌ | ✅ | ❌ | ✅ |
| `object.acl` | ✅ | ✅ | ❌ | ✅ |
| `object.holds` | ❌ | ✅ | ❌ | ✅ |
| `object.retention` | ❌ | ❌ | ❌ | ✅ |
| `object.custom_time` | ✅ | ✅ | ✅ | ✅ |
| `object.storage_class_on_write` | ✅ | ✅ | ✅ | ✅ |
| `object.gzip_transcoding` | ✅ | ✅ | ❌ | ✅ |
| `object.customer_supplied_key` | ❌ | ✅ | ❌ | ✅ |
| `object.csek_rotation` | ❌ | ❌ | ❌ | ✅ |
| `object.soft_delete_restore` | ❌ | ✅ | ✅ | ✅ |
| `signed.v4_get` | ✅ | ✅ | ✅ | ✅ |
| `signed.v4_put` | ✅ | ✅ | ✅ | ✅ |
| `signed.v4_resumable_upload` | ❌ | ❌ | ❌ | ✅ |
| `signed.v4_response_overrides` | ✅ | ✅ | ❌ | ✅ |
| `signed.v2_get` | ✅ | ✅ | ✅ | ✅ |
| `signed.post_policy_v4` | ❌ | ❌ | ❌ | ✅ |
| `grpc.bucket_create_get_delete` | ❌ | ✅ | ❌ | ✅ |
| `grpc.write_read` | ❌ | ✅ | ❌ | ✅ |
| `grpc.list` | ❌ | ✅ | ❌ | ✅ |
| `grpc.update_delete` | ❌ | ✅ | ❌ | ✅ |
| `grpc.multi_range_read` | ❌ | ❌ | ❌ | ✅ |
| `grpc.appendable_object` | ❌ | ❌ | ❌ | ✅ |
| `grpc.checksum_rejected` | ❌ | ❌ | ❌ | ✅ |
| `grpc.precondition_failed` | ❌ | ✅ | ❌ | ✅ |
| `sdk.json_reads` | ✅ | ✅ | ✅ | ✅ |
| `sdk.writer_progress` | ✅ | ✅ | ❌ | ✅ |
| `sdk.pager_resume` | ✅ | ✅ | ❌ | ✅ |

### Ruby

| Case | fake-gcs-server | floci-gcp | gcp-storage-emulator | gcs-mimic |
| --- | --- | --- | --- | --- |
| `bucket.create_get_delete` | ✅ | ✅ | ✅ | ✅ |
| `bucket.list_prefix_paging` | ✅ | ✅ | ❌ | ✅ |
| `bucket.update_labels` | ❌ | ❌ | ❌ | ✅ |
| `bucket.metageneration_precondition` | ❌ | ❌ | ❌ | ✅ |
| `bucket.versioning_history` | ✅ | ✅ | ❌ | ✅ |
| `bucket.lifecycle` | ❌ | ✅ | ✅ | ✅ |
| `bucket.cors` | ❌ | ✅ | ✅ | ✅ |
| `bucket.retention_policy` | ❌ | ✅ | ❌ | ✅ |
| `bucket.retention_lock` | ❌ | ✅ | ❌ | ✅ |
| `bucket.uniform_bucket_level_access` | ❌ | ✅ | ✅ | ✅ |
| `bucket.public_access_prevention` | ❌ | ✅ | ✅ | ✅ |
| `bucket.iam_policy` | ❌ | ✅ | ✅ | ✅ |
| `bucket.iam_test_permissions` | ❌ | ✅ | ✅ | ✅ |
| `bucket.soft_delete_policy` | ❌ | ✅ | ✅ | ✅ |
| `bucket.soft_delete_restore` | ❌ | ❌ | ❌ | ✅ |
| `bucket.autoclass` | ❌ | ❌ | ❌ | ✅ |
| `bucket.acl` | ✅ | ✅ | ❌ | ✅ |
| `bucket.default_object_acl` | ❌ | ✅ | ❌ | ✅ |
| `bucket.notifications` | ✅ | ✅ | ❌ | ✅ |
| `project.hmac_keys` | ❌ | ❌ | ❌ | ✅ |
| `project.service_account` | ❌ | ✅ | ❌ | ✅ |
| `object.write_read_small` | ✅ | ✅ | ✅ | ✅ |
| `object.resumable_large` | ✅ | ✅ | ✅ | ✅ |
| `object.metadata_on_write` | ❌ | ✅ | ✅ | ✅ |
| `object.checksum_rejected` | ✅ | ❌ | ✅ | ✅ |
| `object.md5_accepted` | ✅ | ✅ | ✅ | ✅ |
| `object.range_reads` | ✅ | ✅ | ❌ | ✅ |
| `object.update_metadata` | ❌ | ✅ | ❌ | ✅ |
| `object.delete_not_found` | ✅ | ✅ | ✅ | ✅ |
| `object.does_not_exist_precondition` | ✅ | ✅ | ❌ | ✅ |
| `object.generation_match_precondition` | ❌ | ✅ | ❌ | ✅ |
| `object.metageneration_match_precondition` | ❌ | ✅ | ❌ | ✅ |
| `object.list_prefix_delimiter` | ✅ | ✅ | ✅ | ✅ |
| `object.list_paging` | ✅ | ✅ | ❌ | ✅ |
| `object.list_offsets` | n/a | n/a | n/a | n/a |
| `object.list_match_glob` | ❌ | ✅ | ✅ | ✅ |
| `object.copy_with_metadata` | ✅ | ❌ | ❌ | ✅ |
| `object.rewrite_storage_class` | ❌ | ✅ | ❌ | ✅ |
| `object.compose` | ❌ | ✅ | ❌ | ✅ |
| `object.compose_preconditions` | ❌ | ❌ | ❌ | ✅ |
| `object.move` | ❌ | ✅ | ❌ | ✅ |
| `object.acl` | ✅ | ✅ | ❌ | ✅ |
| `object.holds` | ❌ | ✅ | ❌ | ✅ |
| `object.retention` | ❌ | ❌ | ❌ | ✅ |
| `object.custom_time` | ✅ | ✅ | ✅ | ✅ |
| `object.storage_class_on_write` | ✅ | ✅ | ✅ | ✅ |
| `object.gzip_transcoding` | ✅ | ✅ | ❌ | ✅ |
| `object.customer_supplied_key` | ❌ | ✅ | ❌ | ✅ |
| `object.csek_rotation` | ❌ | ❌ | ❌ | ✅ |
| `object.soft_delete_restore` | ❌ | ✅ | ✅ | ✅ |
| `signed.v4_get` | ✅ | ✅ | ✅ | ✅ |
| `signed.v4_put` | ✅ | ✅ | ✅ | ✅ |
| `signed.v4_resumable_upload` | ❌ | ❌ | ❌ | ✅ |
| `signed.v4_response_overrides` | ❌ | ✅ | ❌ | ✅ |
| `signed.v2_get` | ✅ | ✅ | ✅ | ✅ |
| `signed.post_policy_v4` | ✅ | ❌ | ❌ | ✅ |
| `sdk.file_upload_download` | ✅ | ✅ | ✅ | ✅ |
| `sdk.download_to_io` | ✅ | ✅ | ✅ | ✅ |

### Node.js

| Case | fake-gcs-server | floci-gcp | gcp-storage-emulator | gcs-mimic |
| --- | --- | --- | --- | --- |
| `bucket.create_get_delete` | ✅ | ✅ | ✅ | ✅ |
| `bucket.list_prefix_paging` | ✅ | ✅ | ❌ | ✅ |
| `bucket.update_labels` | ❌ | ✅ | ❌ | ✅ |
| `bucket.metageneration_precondition` | ❌ | ❌ | ❌ | ✅ |
| `bucket.versioning_history` | ✅ | ✅ | ❌ | ✅ |
| `bucket.lifecycle` | ❌ | ✅ | ✅ | ✅ |
| `bucket.cors` | ❌ | ✅ | ✅ | ✅ |
| `bucket.uniform_bucket_level_access` | ❌ | ✅ | ✅ | ✅ |
| `bucket.public_access_prevention` | ❌ | ✅ | ✅ | ✅ |
| `bucket.soft_delete_policy` | ❌ | ✅ | ✅ | ✅ |
| `bucket.soft_delete_restore` | ❌ | ❌ | ❌ | ✅ |
| `bucket.autoclass` | ❌ | ❌ | ❌ | ✅ |
| `bucket.retention_policy` | ❌ | ✅ | ❌ | ✅ |
| `bucket.retention_lock` | ❌ | ✅ | ❌ | ✅ |
| `bucket.iam_policy` | ❌ | ✅ | ✅ | ✅ |
| `bucket.iam_test_permissions` | ❌ | ✅ | ✅ | ✅ |
| `bucket.acl` | ✅ | ✅ | ❌ | ✅ |
| `bucket.default_object_acl` | ❌ | ✅ | ❌ | ✅ |
| `bucket.notifications` | ✅ | ✅ | ❌ | ✅ |
| `project.hmac_keys` | ❌ | ✅ | ❌ | ✅ |
| `project.service_account` | ❌ | ✅ | ❌ | ✅ |
| `object.write_read_small` | ✅ | ✅ | ✅ | ✅ |
| `object.resumable_large` | ✅ | ✅ | ✅ | ✅ |
| `object.metadata_on_write` | ✅ | ✅ | ✅ | ✅ |
| `object.checksum_rejected` | ✅ | ❌ | ✅ | ✅ |
| `object.md5_accepted` | ✅ | ✅ | ✅ | ✅ |
| `object.range_reads` | ✅ | ✅ | ❌ | ✅ |
| `object.update_metadata` | ❌ | ✅ | ❌ | ✅ |
| `object.delete_not_found` | ✅ | ✅ | ✅ | ✅ |
| `object.does_not_exist_precondition` | ✅ | ✅ | ❌ | ✅ |
| `object.generation_match_precondition` | ❌ | ✅ | ❌ | ✅ |
| `object.metageneration_match_precondition` | ❌ | ✅ | ❌ | ✅ |
| `object.list_prefix_delimiter` | ✅ | ✅ | ✅ | ✅ |
| `object.list_paging` | ✅ | ✅ | ❌ | ✅ |
| `object.list_offsets` | ✅ | ✅ | ❌ | ✅ |
| `object.list_match_glob` | ❌ | ✅ | ✅ | ✅ |
| `object.copy_with_metadata` | ✅ | ❌ | ❌ | ✅ |
| `object.rewrite_storage_class` | ❌ | ✅ | ❌ | ✅ |
| `object.compose` | ❌ | ✅ | ❌ | ✅ |
| `object.compose_preconditions` | ❌ | ❌ | ❌ | ✅ |
| `object.move` | ❌ | ✅ | ❌ | ✅ |
| `object.acl` | ✅ | ✅ | ❌ | ✅ |
| `object.holds` | ❌ | ✅ | ❌ | ✅ |
| `object.retention` | ❌ | ❌ | ❌ | ✅ |
| `object.custom_time` | ✅ | ✅ | ✅ | ✅ |
| `object.storage_class_on_write` | ✅ | ✅ | ✅ | ✅ |
| `object.gzip_transcoding` | ✅ | ✅ | ❌ | ✅ |
| `object.customer_supplied_key` | ❌ | ✅ | ❌ | ✅ |
| `object.csek_rotation` | ❌ | ❌ | ❌ | ✅ |
| `object.soft_delete_restore` | ❌ | ✅ | ✅ | ✅ |
| `signed.v4_get` | ✅ | ✅ | ✅ | ✅ |
| `signed.v4_put` | ✅ | ✅ | ✅ | ✅ |
| `signed.v4_resumable_upload` | ❌ | ❌ | ❌ | ✅ |
| `signed.v4_response_overrides` | ✅ | ✅ | ❌ | ✅ |
| `signed.v2_get` | ✅ | ✅ | ✅ | ✅ |
| `signed.post_policy_v4` | ✅ | ❌ | ❌ | ✅ |
| `sdk.resumable_resume` | ❌ | ✅ | ✅ | ✅ |
| `sdk.sliced_download` | ✅ | ✅ | ✅ | ✅ |
| `sdk.file_upload_download` | ✅ | ✅ | ✅ | ✅ |

### PHP

| Case | fake-gcs-server | floci-gcp | gcp-storage-emulator | gcs-mimic |
| --- | --- | --- | --- | --- |
| `bucket.create_get_delete` | ✅ | ✅ | ✅ | ✅ |
| `bucket.list_prefix_paging` | ✅ | ✅ | ❌ | ✅ |
| `bucket.update_labels` | ❌ | ✅ | ❌ | ✅ |
| `bucket.metageneration_precondition` | ❌ | ❌ | ❌ | ✅ |
| `bucket.versioning_history` | ✅ | ✅ | ❌ | ✅ |
| `bucket.lifecycle` | ❌ | ✅ | ✅ | ✅ |
| `bucket.cors` | ❌ | ✅ | ✅ | ✅ |
| `bucket.soft_delete_policy` | ❌ | ✅ | ✅ | ✅ |
| `bucket.soft_delete_restore` | ❌ | ❌ | ❌ | ✅ |
| `bucket.autoclass` | ❌ | ❌ | ❌ | ✅ |
| `bucket.retention_policy` | ❌ | ❌ | ❌ | ✅ |
| `bucket.retention_lock` | ❌ | ✅ | ❌ | ✅ |
| `bucket.uniform_bucket_level_access` | ❌ | ✅ | ✅ | ✅ |
| `bucket.public_access_prevention` | ❌ | ✅ | ✅ | ✅ |
| `bucket.iam_policy` | ❌ | ✅ | ✅ | ✅ |
| `bucket.iam_test_permissions` | ❌ | ✅ | ✅ | ✅ |
| `bucket.acl` | ✅ | ✅ | ❌ | ✅ |
| `bucket.default_object_acl` | ❌ | ✅ | ❌ | ✅ |
| `bucket.notifications` | ✅ | ✅ | ❌ | ✅ |
| `project.hmac_keys` | ❌ | ✅ | ❌ | ✅ |
| `project.service_account` | ❌ | ✅ | ❌ | ✅ |
| `object.write_read_small` | ✅ | ✅ | ✅ | ✅ |
| `object.resumable_large` | ✅ | ✅ | ✅ | ✅ |
| `object.metadata_on_write` | ✅ | ✅ | ✅ | ✅ |
| `object.checksum_rejected` | ✅ | ❌ | ❌ | ✅ |
| `object.md5_accepted` | ✅ | ✅ | ✅ | ✅ |
| `object.range_reads` | ✅ | ✅ | ❌ | ✅ |
| `object.update_metadata` | ❌ | ✅ | ❌ | ✅ |
| `object.delete_not_found` | ✅ | ✅ | ❌ | ✅ |
| `object.does_not_exist_precondition` | ✅ | ✅ | ❌ | ✅ |
| `object.generation_match_precondition` | ❌ | ✅ | ❌ | ✅ |
| `object.metageneration_match_precondition` | ❌ | ✅ | ❌ | ✅ |
| `object.list_prefix_delimiter` | ✅ | ✅ | ✅ | ✅ |
| `object.list_paging` | ✅ | ✅ | ❌ | ✅ |
| `object.list_offsets` | ✅ | ✅ | ❌ | ✅ |
| `object.list_match_glob` | ❌ | ✅ | ✅ | ✅ |
| `object.copy_with_metadata` | ✅ | ❌ | ❌ | ✅ |
| `object.rewrite_storage_class` | ❌ | ✅ | ❌ | ✅ |
| `object.compose` | ❌ | ✅ | ❌ | ✅ |
| `object.compose_preconditions` | ❌ | ❌ | ❌ | ✅ |
| `object.move` | ❌ | ✅ | ❌ | ✅ |
| `object.acl` | ✅ | ✅ | ❌ | ✅ |
| `object.holds` | ❌ | ❌ | ❌ | ✅ |
| `object.retention` | ❌ | ❌ | ❌ | ✅ |
| `object.custom_time` | ✅ | ✅ | ✅ | ✅ |
| `object.storage_class_on_write` | ✅ | ✅ | ✅ | ✅ |
| `object.gzip_transcoding` | ✅ | ✅ | ❌ | ✅ |
| `object.customer_supplied_key` | ❌ | ✅ | ❌ | ✅ |
| `object.csek_rotation` | ❌ | ❌ | ❌ | ✅ |
| `object.soft_delete_restore` | ❌ | ✅ | ✅ | ✅ |
| `signed.v4_get` | ✅ | ✅ | ✅ | ✅ |
| `signed.v4_put` | ✅ | ✅ | ✅ | ✅ |
| `signed.v4_resumable_upload` | ❌ | ❌ | ❌ | ✅ |
| `signed.v4_response_overrides` | ✅ | ✅ | ❌ | ✅ |
| `signed.v2_get` | ✅ | ✅ | ✅ | ✅ |
| `signed.post_policy_v4` | ✅ | ❌ | ❌ | ✅ |
| `sdk.resumable_resume` | ❌ | ✅ | ❌ | ✅ |
| `sdk.file_upload_download` | ✅ | ✅ | ✅ | ✅ |
| `sdk.stream_wrapper` | ✅ | ✅ | ❌ | ✅ |
| `sdk.download_as_stream` | ✅ | ✅ | ✅ | ✅ |

### Python

| Case | fake-gcs-server | floci-gcp | gcp-storage-emulator | gcs-mimic |
| --- | --- | --- | --- | --- |
| `bucket.create_get_delete` | ✅ | ✅ | ✅ | ✅ |
| `bucket.list_prefix_paging` | ✅ | ✅ | ❌ | ✅ |
| `bucket.update_labels` | ❌ | ✅ | ✅ | ✅ |
| `bucket.metageneration_precondition` | ❌ | ❌ | ❌ | ✅ |
| `bucket.versioning_history` | ❌ | ✅ | ❌ | ✅ |
| `bucket.lifecycle` | ❌ | ✅ | ✅ | ✅ |
| `bucket.cors` | ❌ | ✅ | ✅ | ✅ |
| `bucket.retention_policy` | ❌ | ❌ | ❌ | ✅ |
| `bucket.retention_lock` | ❌ | ✅ | ❌ | ✅ |
| `bucket.uniform_bucket_level_access` | ❌ | ✅ | ✅ | ✅ |
| `bucket.public_access_prevention` | ❌ | ✅ | ✅ | ✅ |
| `bucket.iam_policy` | ❌ | ✅ | ✅ | ✅ |
| `bucket.iam_test_permissions` | ❌ | ✅ | ✅ | ✅ |
| `bucket.soft_delete_policy` | ❌ | ✅ | ✅ | ✅ |
| `bucket.soft_delete_restore` | ❌ | ❌ | ❌ | ✅ |
| `bucket.autoclass` | ❌ | ❌ | ❌ | ✅ |
| `bucket.acl` | ❌ | ❌ | ✅ | ✅ |
| `bucket.default_object_acl` | ❌ | ❌ | ✅ | ✅ |
| `bucket.notifications` | ✅ | ✅ | ❌ | ✅ |
| `project.hmac_keys` | ❌ | ✅ | ❌ | ✅ |
| `project.service_account` | ❌ | ✅ | ❌ | ✅ |
| `object.write_read_small` | ❌ | ✅ | ❌ | ✅ |
| `object.resumable_large` | ✅ | ✅ | ❌ | ✅ |
| `object.metadata_on_write` | ✅ | ✅ | ✅ | ✅ |
| `object.checksum_rejected` | ✅ | ❌ | ✅ | ✅ |
| `object.md5_accepted` | ✅ | ✅ | ✅ | ✅ |
| `object.range_reads` | ❌ | ✅ | ❌ | ✅ |
| `object.update_metadata` | ❌ | ✅ | ❌ | ✅ |
| `object.delete_not_found` | ✅ | ✅ | ✅ | ✅ |
| `object.does_not_exist_precondition` | ❌ | ✅ | ❌ | ✅ |
| `object.generation_match_precondition` | ❌ | ✅ | ❌ | ✅ |
| `object.metageneration_match_precondition` | ❌ | ✅ | ❌ | ✅ |
| `object.list_prefix_delimiter` | ✅ | ✅ | ✅ | ✅ |
| `object.list_paging` | ✅ | ✅ | ❌ | ✅ |
| `object.list_offsets` | ✅ | ✅ | ❌ | ✅ |
| `object.list_match_glob` | ❌ | ✅ | ✅ | ✅ |
| `object.copy_with_metadata` | ✅ | ❌ | ❌ | ✅ |
| `object.rewrite_storage_class` | ❌ | ✅ | ❌ | ✅ |
| `object.compose` | ❌ | ✅ | ❌ | ✅ |
| `object.compose_preconditions` | ❌ | ❌ | ❌ | ✅ |
| `object.move` | ❌ | ✅ | ❌ | ✅ |
| `object.acl` | ✅ | ❌ | ✅ | ✅ |
| `object.holds` | ❌ | ❌ | ❌ | ✅ |
| `object.retention` | ❌ | ❌ | ❌ | ✅ |
| `object.custom_time` | ✅ | ✅ | ✅ | ✅ |
| `object.storage_class_on_write` | ✅ | ✅ | ✅ | ✅ |
| `object.gzip_transcoding` | ❌ | ✅ | ❌ | ✅ |
| `object.customer_supplied_key` | ❌ | ✅ | ❌ | ✅ |
| `object.csek_rotation` | ❌ | ❌ | ❌ | ✅ |
| `object.soft_delete_restore` | ❌ | ✅ | ✅ | ✅ |
| `signed.v4_get` | ✅ | ✅ | ✅ | ✅ |
| `signed.v4_put` | ✅ | ✅ | ✅ | ✅ |
| `signed.v4_resumable_upload` | ❌ | ❌ | ❌ | ✅ |
| `signed.v4_response_overrides` | ✅ | ✅ | ❌ | ✅ |
| `signed.v2_get` | ✅ | ✅ | ✅ | ✅ |
| `signed.post_policy_v4` | ✅ | ❌ | ❌ | ✅ |
| `grpc.multi_range_read` | ❌ | ❌ | ❌ | ✅ |
| `grpc.appendable_object` | ❌ | ❌ | ❌ | ✅ |
| `sdk.object_update_replace` | ✅ | ✅ | ❌ | ✅ |
| `sdk.json_batch` | ❌ | ✅ | ❌ | ✅ |
| `sdk.sliced_download` | ✅ | ❌ | ❌ | ✅ |
| `sdk.xml_multipart_upload` | ❌ | ❌ | ❌ | ✅ |
| `sdk.file_upload_download` | ✅ | ✅ | ✅ | ✅ |

### Java

| Case | fake-gcs-server | floci-gcp | gcp-storage-emulator | gcs-mimic |
| --- | --- | --- | --- | --- |
| `bucket.create_get_delete` | ✅ | ✅ | ✅ | ✅ |
| `bucket.list_prefix_paging` | ✅ | ✅ | ❌ | ✅ |
| `bucket.update_labels` | ❌ | ✅ | ❌ | ✅ |
| `bucket.metageneration_precondition` | ❌ | ❌ | ❌ | ✅ |
| `bucket.versioning_history` | ✅ | ✅ | ❌ | ✅ |
| `bucket.lifecycle` | ❌ | ✅ | ✅ | ✅ |
| `bucket.cors` | ❌ | ✅ | ✅ | ✅ |
| `bucket.retention_policy` | ❌ | ❌ | ❌ | ✅ |
| `bucket.retention_lock` | ❌ | ✅ | ❌ | ✅ |
| `bucket.uniform_bucket_level_access` | ❌ | ❌ | ✅ | ✅ |
| `bucket.public_access_prevention` | ❌ | ❌ | ✅ | ✅ |
| `bucket.iam_policy` | ❌ | ✅ | ✅ | ✅ |
| `bucket.iam_test_permissions` | ❌ | ✅ | ✅ | ✅ |
| `bucket.soft_delete_policy` | ❌ | ✅ | ✅ | ✅ |
| `bucket.soft_delete_restore` | n/a | n/a | n/a | n/a |
| `bucket.autoclass` | ❌ | ❌ | ❌ | ✅ |
| `bucket.acl` | ❌ | ❌ | ✅ | ✅ |
| `bucket.default_object_acl` | ❌ | ❌ | ✅ | ✅ |
| `bucket.notifications` | ✅ | ✅ | ❌ | ✅ |
| `project.hmac_keys` | ❌ | ✅ | ❌ | ✅ |
| `project.service_account` | ❌ | ✅ | ❌ | ✅ |
| `object.write_read_small` | ✅ | ✅ | ✅ | ✅ |
| `object.resumable_large` | ✅ | ✅ | ✅ | ✅ |
| `object.metadata_on_write` | ✅ | ✅ | ✅ | ✅ |
| `object.checksum_rejected` | ✅ | ❌ | ✅ | ✅ |
| `object.md5_accepted` | ✅ | ✅ | ✅ | ✅ |
| `object.range_reads` | ✅ | ✅ | ❌ | ✅ |
| `object.update_metadata` | ❌ | ✅ | ❌ | ✅ |
| `object.delete_not_found` | ✅ | ✅ | ✅ | ✅ |
| `object.does_not_exist_precondition` | ✅ | ✅ | ❌ | ✅ |
| `object.generation_match_precondition` | ❌ | ✅ | ❌ | ✅ |
| `object.metageneration_match_precondition` | ❌ | ✅ | ❌ | ✅ |
| `object.list_prefix_delimiter` | ✅ | ✅ | ✅ | ✅ |
| `object.list_paging` | ✅ | ✅ | ❌ | ✅ |
| `object.list_offsets` | ✅ | ✅ | ❌ | ✅ |
| `object.list_match_glob` | ❌ | ✅ | ✅ | ✅ |
| `object.copy_with_metadata` | ✅ | ❌ | ❌ | ✅ |
| `object.rewrite_storage_class` | ❌ | ✅ | ❌ | ✅ |
| `object.compose` | ❌ | ✅ | ❌ | ✅ |
| `object.compose_preconditions` | ❌ | ❌ | ❌ | ✅ |
| `object.move` | ❌ | ✅ | ❌ | ✅ |
| `object.acl` | ✅ | ❌ | ✅ | ✅ |
| `object.holds` | ❌ | ✅ | ❌ | ✅ |
| `object.retention` | ❌ | ❌ | ❌ | ✅ |
| `object.custom_time` | ✅ | ✅ | ✅ | ✅ |
| `object.storage_class_on_write` | ✅ | ✅ | ✅ | ✅ |
| `object.gzip_transcoding` | ✅ | ✅ | ❌ | ✅ |
| `object.customer_supplied_key` | ❌ | ✅ | ❌ | ✅ |
| `object.csek_rotation` | ❌ | ❌ | ❌ | ✅ |
| `object.soft_delete_restore` | ❌ | ✅ | ✅ | ✅ |
| `signed.v4_get` | ✅ | ✅ | ✅ | ✅ |
| `signed.v4_put` | ✅ | ✅ | ✅ | ✅ |
| `signed.v4_resumable_upload` | ❌ | ❌ | ❌ | ✅ |
| `signed.v4_response_overrides` | ✅ | ✅ | ❌ | ✅ |
| `signed.v2_get` | ✅ | ✅ | ✅ | ✅ |
| `signed.post_policy_v4` | ✅ | ❌ | ❌ | ✅ |
| `grpc.bucket_create_get_delete` | ❌ | ✅ | ❌ | ✅ |
| `grpc.write_read` | ❌ | ✅ | ❌ | ✅ |
| `grpc.list` | ❌ | ✅ | ❌ | ✅ |
| `grpc.update_delete` | ❌ | ✅ | ❌ | ✅ |
| `grpc.multi_range_read` | ❌ | ❌ | ❌ | ✅ |
| `grpc.appendable_object` | ❌ | ❌ | ❌ | ✅ |
| `grpc.checksum_rejected` | ❌ | ❌ | ❌ | ✅ |
| `grpc.precondition_failed` | ❌ | ✅ | ❌ | ✅ |
| `grpc.write_session_bidi` | ❌ | ✅ | ❌ | ✅ |
| `grpc.write_session_journaling` | ❌ | ✅ | ❌ | ✅ |
| `sdk.blob_handle_roundtrip` | ✅ | ✅ | ✅ | ✅ |
| `sdk.default_upload_crc32c` | ✅ | ❌ | ✅ | ✅ |
| `sdk.file_upload_download` | ✅ | ✅ | ✅ | ✅ |
| `sdk.write_channel_streaming` | ✅ | ✅ | ✅ | ✅ |
| `sdk.read_channel_chunked` | ✅ | ✅ | ✅ | ✅ |
| `sdk.list_iterate_all` | ❌ | ✅ | ❌ | ✅ |
| `sdk.json_batch` | ✅ | ✅ | ❌ | ✅ |
| `sdk.resumable_resume` | ✅ | ✅ | ✅ | ✅ |
| `sdk.blob_write_session_default` | ✅ | ✅ | ✅ | ✅ |
| `sdk.transfer_manager_upload` | ✅ | ✅ | ✅ | ✅ |
| `sdk.sliced_download` | ✅ | ✅ | ✅ | ✅ |
| `sdk.parallel_composite_upload` | ❌ | ❌ | ❌ | ✅ |
| `sdk.write_session_parallel_composite` | ❌ | ❌ | ❌ | ✅ |
| `sdk.write_session_buffer_to_disk` | ✅ | ✅ | ✅ | ✅ |

### .NET

| Case | fake-gcs-server | floci-gcp | gcp-storage-emulator | gcs-mimic |
| --- | --- | --- | --- | --- |
| `bucket.create_get_delete` | ✅ | ✅ | ✅ | ✅ |
| `bucket.list_prefix_paging` | ✅ | ✅ | ❌ | ✅ |
| `bucket.update_labels` | ❌ | ✅ | ❌ | ✅ |
| `bucket.metageneration_precondition` | ❌ | ❌ | ❌ | ✅ |
| `bucket.versioning_history` | ✅ | ✅ | ❌ | ✅ |
| `bucket.lifecycle` | ❌ | ✅ | ✅ | ✅ |
| `bucket.cors` | ❌ | ✅ | ✅ | ✅ |
| `bucket.retention_policy` | ❌ | ✅ | ❌ | ✅ |
| `bucket.retention_lock` | ❌ | ✅ | ❌ | ✅ |
| `bucket.uniform_bucket_level_access` | ❌ | ✅ | ✅ | ✅ |
| `bucket.public_access_prevention` | ❌ | ✅ | ✅ | ✅ |
| `bucket.iam_policy` | ❌ | ✅ | ✅ | ✅ |
| `bucket.iam_test_permissions` | ❌ | ✅ | ✅ | ✅ |
| `bucket.soft_delete_policy` | ❌ | ✅ | ✅ | ✅ |
| `bucket.soft_delete_restore` | ❌ | ❌ | ❌ | ✅ |
| `bucket.autoclass` | ❌ | ❌ | ❌ | ✅ |
| `bucket.acl` | ❌ | ❌ | ✅ | ✅ |
| `bucket.default_object_acl` | ❌ | ❌ | ✅ | ✅ |
| `bucket.notifications` | ✅ | ✅ | ❌ | ✅ |
| `project.hmac_keys` | ❌ | ✅ | ❌ | ✅ |
| `project.service_account` | ❌ | ✅ | ❌ | ✅ |
| `object.write_read_small` | ✅ | ✅ | ✅ | ✅ |
| `object.resumable_large` | ✅ | ✅ | ✅ | ✅ |
| `object.metadata_on_write` | ✅ | ✅ | ✅ | ✅ |
| `object.checksum_rejected` | ✅ | ❌ | ✅ | ✅ |
| `object.md5_accepted` | ✅ | ✅ | ✅ | ✅ |
| `object.range_reads` | ❌ | ❌ | ❌ | ✅ |
| `object.update_metadata` | ❌ | ✅ | ❌ | ✅ |
| `object.delete_not_found` | ✅ | ✅ | ✅ | ✅ |
| `object.does_not_exist_precondition` | ✅ | ✅ | ❌ | ✅ |
| `object.generation_match_precondition` | ❌ | ✅ | ❌ | ✅ |
| `object.metageneration_match_precondition` | ❌ | ✅ | ❌ | ✅ |
| `object.list_prefix_delimiter` | ✅ | ✅ | ✅ | ✅ |
| `object.list_paging` | ✅ | ✅ | ❌ | ✅ |
| `object.list_offsets` | ✅ | ✅ | ❌ | ✅ |
| `object.list_match_glob` | ❌ | ✅ | ✅ | ✅ |
| `object.copy_with_metadata` | ✅ | ❌ | ❌ | ✅ |
| `object.rewrite_storage_class` | ❌ | ✅ | ❌ | ✅ |
| `object.compose` | ❌ | ✅ | ❌ | ✅ |
| `object.compose_preconditions` | ❌ | ❌ | ❌ | ✅ |
| `object.move` | ❌ | ✅ | ❌ | ✅ |
| `object.acl` | ✅ | ❌ | ✅ | ✅ |
| `object.holds` | ❌ | ✅ | ❌ | ✅ |
| `object.retention` | ❌ | ❌ | ❌ | ✅ |
| `object.custom_time` | ✅ | ✅ | ✅ | ✅ |
| `object.storage_class_on_write` | ✅ | ✅ | ✅ | ✅ |
| `object.gzip_transcoding` | n/a | n/a | n/a | n/a |
| `object.customer_supplied_key` | ❌ | ✅ | ❌ | ✅ |
| `object.csek_rotation` | ❌ | ❌ | ❌ | ✅ |
| `object.soft_delete_restore` | ❌ | ✅ | ✅ | ✅ |
| `signed.v4_get` | ✅ | ✅ | ✅ | ✅ |
| `signed.v4_put` | ✅ | ✅ | ✅ | ✅ |
| `signed.v4_resumable_upload` | ❌ | ❌ | ❌ | ✅ |
| `signed.v4_response_overrides` | ✅ | ✅ | ❌ | ✅ |
| `signed.v2_get` | ✅ | ✅ | ✅ | ✅ |
| `signed.post_policy_v4` | ❌ | ❌ | ❌ | ✅ |
| `sdk.download_hash_validation` | ✅ | ✅ | ✅ | ✅ |
| `sdk.download_chunked` | ✅ | ✅ | ✅ | ✅ |
| `sdk.update_object_implicit_preconditions` | ❌ | ❌ | ❌ | ✅ |
| `sdk.update_bucket_etag_ignored` | ❌ | ❌ | ❌ | ✅ |
| `sdk.iam_policy_etag` | ❌ | ❌ | ❌ | ✅ |
| `sdk.resumable_resume` | ❌ | ✅ | ✅ | ✅ |
| `sdk.object_uploader_session` | ✅ | ✅ | ✅ | ✅ |
| `sdk.upload_progress` | ✅ | ✅ | ✅ | ✅ |
| `sdk.download_gzip_default` | ✅ | ✅ | ❌ | ✅ |
| `sdk.list_auto_paging` | ✅ | ✅ | ❌ | ✅ |
| `sdk.conditional_replace_and_read` | ❌ | ✅ | ❌ | ✅ |
| `sdk.delete_bucket_with_objects` | ✅ | ✅ | ✅ | ✅ |
| `sdk.list_fields_partial` | ❌ | ❌ | ❌ | ✅ |
| `sdk.signed_request_template_headers` | ✅ | ✅ | ❌ | ✅ |
| `sdk.signed_query_generation` | ✅ | ✅ | ❌ | ✅ |
| `sdk.rewrite_token_loop` | ❌ | ✅ | ❌ | ✅ |
| `sdk.upload_predefined_acl` | ✅ | ❌ | ❌ | ✅ |

### Rust

| Case | fake-gcs-server | floci-gcp | gcp-storage-emulator | gcs-mimic |
| --- | --- | --- | --- | --- |
| `bucket.create_get_delete` | ❌ | ✅ | ❌ | ✅ |
| `bucket.list_prefix_paging` | ❌ | ✅ | ❌ | ✅ |
| `bucket.update_labels` | ❌ | ✅ | ❌ | ✅ |
| `bucket.metageneration_precondition` | ❌ | ✅ | ❌ | ✅ |
| `bucket.versioning_history` | ❌ | ✅ | ❌ | ✅ |
| `bucket.lifecycle` | ❌ | ❌ | ❌ | ✅ |
| `bucket.cors` | ❌ | ❌ | ❌ | ✅ |
| `bucket.retention_policy` | ❌ | ❌ | ❌ | ✅ |
| `bucket.retention_lock` | ❌ | ❌ | ❌ | ✅ |
| `bucket.uniform_bucket_level_access` | ❌ | ✅ | ❌ | ✅ |
| `bucket.public_access_prevention` | ❌ | ✅ | ❌ | ✅ |
| `bucket.iam_policy` | ❌ | ❌ | ❌ | ✅ |
| `bucket.iam_test_permissions` | ❌ | ❌ | ❌ | ✅ |
| `bucket.soft_delete_policy` | ❌ | ✅ | ❌ | ✅ |
| `bucket.soft_delete_restore` | n/a | n/a | n/a | n/a |
| `bucket.autoclass` | ❌ | ❌ | ❌ | ✅ |
| `bucket.acl` | ❌ | ❌ | ❌ | ✅ |
| `bucket.default_object_acl` | ❌ | ❌ | ❌ | ✅ |
| `bucket.notifications` | n/a | n/a | n/a | n/a |
| `project.hmac_keys` | n/a | n/a | n/a | n/a |
| `project.service_account` | n/a | n/a | n/a | n/a |
| `object.write_read_small` | ❌ | ✅ | ❌ | ✅ |
| `object.resumable_large` | ❌ | ✅ | ❌ | ✅ |
| `object.metadata_on_write` | ❌ | ✅ | ❌ | ✅ |
| `object.checksum_rejected` | ❌ | ❌ | ❌ | ✅ |
| `object.md5_accepted` | ❌ | ✅ | ❌ | ✅ |
| `object.range_reads` | ❌ | ✅ | ❌ | ✅ |
| `object.update_metadata` | ❌ | ✅ | ❌ | ✅ |
| `object.delete_not_found` | ❌ | ✅ | ❌ | ✅ |
| `object.does_not_exist_precondition` | ❌ | ✅ | ❌ | ✅ |
| `object.generation_match_precondition` | ❌ | ✅ | ❌ | ✅ |
| `object.metageneration_match_precondition` | ❌ | ✅ | ❌ | ✅ |
| `object.list_prefix_delimiter` | ❌ | ✅ | ❌ | ✅ |
| `object.list_paging` | ❌ | ✅ | ❌ | ✅ |
| `object.list_offsets` | ❌ | ✅ | ❌ | ✅ |
| `object.list_match_glob` | ❌ | ❌ | ❌ | ✅ |
| `object.copy_with_metadata` | ❌ | ❌ | ❌ | ✅ |
| `object.rewrite_storage_class` | ❌ | ❌ | ❌ | ✅ |
| `object.compose` | ❌ | ✅ | ❌ | ✅ |
| `object.compose_preconditions` | ❌ | ✅ | ❌ | ✅ |
| `object.move` | ❌ | ❌ | ❌ | ✅ |
| `object.acl` | ❌ | ❌ | ❌ | ✅ |
| `object.holds` | ❌ | ✅ | ❌ | ✅ |
| `object.retention` | ❌ | ❌ | ❌ | ✅ |
| `object.custom_time` | ❌ | ✅ | ❌ | ✅ |
| `object.storage_class_on_write` | ❌ | ✅ | ❌ | ✅ |
| `object.gzip_transcoding` | ❌ | ✅ | ❌ | ✅ |
| `object.customer_supplied_key` | ❌ | ✅ | ❌ | ✅ |
| `object.csek_rotation` | ❌ | ❌ | ❌ | ✅ |
| `object.soft_delete_restore` | ❌ | ❌ | ❌ | ✅ |
| `signed.v4_get` | ❌ | ✅ | ❌ | ✅ |
| `signed.v4_put` | ❌ | ✅ | ❌ | ✅ |
| `signed.v4_resumable_upload` | ❌ | ❌ | ❌ | ✅ |
| `signed.v4_response_overrides` | ❌ | ✅ | ❌ | ✅ |
| `signed.v2_get` | n/a | n/a | n/a | n/a |
| `signed.post_policy_v4` | ❌ | ❌ | ❌ | ✅ |
| `grpc.multi_range_read` | ❌ | ❌ | ❌ | ✅ |
| `sdk.rewrite_token_loop` | ❌ | ❌ | ❌ | ✅ |
| `sdk.write_unbuffered` | ❌ | ✅ | ❌ | ✅ |

### C++

| Case | fake-gcs-server | floci-gcp | gcp-storage-emulator | gcs-mimic |
| --- | --- | --- | --- | --- |
| `bucket.create_get_delete` | ✅ | ✅ | ✅ | ✅ |
| `bucket.list_prefix_paging` | ✅ | ✅ | ❌ | ✅ |
| `bucket.update_labels` | ❌ | ✅ | ✅ | ✅ |
| `bucket.metageneration_precondition` | ❌ | ❌ | ❌ | ✅ |
| `bucket.versioning_history` | ✅ | ✅ | ❌ | ✅ |
| `bucket.lifecycle` | ❌ | ✅ | ✅ | ✅ |
| `bucket.cors` | ❌ | ✅ | ✅ | ✅ |
| `bucket.retention_policy` | ❌ | ✅ | ❌ | ✅ |
| `bucket.retention_lock` | ❌ | ✅ | ❌ | ✅ |
| `bucket.uniform_bucket_level_access` | ❌ | ✅ | ✅ | ✅ |
| `bucket.public_access_prevention` | ❌ | ✅ | ✅ | ✅ |
| `bucket.iam_policy` | ❌ | ✅ | ✅ | ✅ |
| `bucket.iam_test_permissions` | ❌ | ✅ | ✅ | ✅ |
| `bucket.soft_delete_policy` | ❌ | ✅ | ✅ | ✅ |
| `bucket.soft_delete_restore` | n/a | n/a | n/a | n/a |
| `bucket.autoclass` | ❌ | ❌ | ❌ | ✅ |
| `bucket.acl` | ✅ | ✅ | ❌ | ✅ |
| `bucket.default_object_acl` | ❌ | ✅ | ❌ | ✅ |
| `bucket.notifications` | ✅ | ✅ | ❌ | ✅ |
| `project.hmac_keys` | ❌ | ✅ | ❌ | ✅ |
| `project.service_account` | ❌ | ✅ | ❌ | ✅ |
| `object.write_read_small` | ✅ | ✅ | ✅ | ✅ |
| `object.resumable_large` | ✅ | ✅ | ✅ | ✅ |
| `object.metadata_on_write` | ✅ | ✅ | ✅ | ✅ |
| `object.checksum_rejected` | ✅ | ❌ | ✅ | ✅ |
| `object.md5_accepted` | ✅ | ✅ | ✅ | ✅ |
| `object.range_reads` | ✅ | ✅ | ❌ | ✅ |
| `object.update_metadata` | ❌ | ✅ | ❌ | ✅ |
| `object.delete_not_found` | ✅ | ✅ | ✅ | ✅ |
| `object.does_not_exist_precondition` | ✅ | ✅ | ❌ | ✅ |
| `object.generation_match_precondition` | ❌ | ✅ | ❌ | ✅ |
| `object.metageneration_match_precondition` | ❌ | ✅ | ❌ | ✅ |
| `object.list_prefix_delimiter` | ✅ | ✅ | ✅ | ✅ |
| `object.list_paging` | ✅ | ✅ | ❌ | ✅ |
| `object.list_offsets` | ✅ | ✅ | ❌ | ✅ |
| `object.list_match_glob` | ❌ | ✅ | ✅ | ✅ |
| `object.copy_with_metadata` | ✅ | ❌ | ❌ | ✅ |
| `object.rewrite_storage_class` | ❌ | ✅ | ❌ | ✅ |
| `object.compose` | ❌ | ✅ | ❌ | ✅ |
| `object.compose_preconditions` | ❌ | ❌ | ❌ | ✅ |
| `object.move` | ❌ | ✅ | ❌ | ✅ |
| `object.acl` | ✅ | ✅ | ❌ | ✅ |
| `object.holds` | ❌ | ✅ | ❌ | ✅ |
| `object.retention` | ❌ | ❌ | ❌ | ✅ |
| `object.custom_time` | ✅ | ✅ | ✅ | ✅ |
| `object.storage_class_on_write` | ✅ | ✅ | ✅ | ✅ |
| `object.gzip_transcoding` | ❌ | ❌ | ❌ | ✅ |
| `object.customer_supplied_key` | ❌ | ✅ | ❌ | ✅ |
| `object.csek_rotation` | ❌ | ❌ | ❌ | ✅ |
| `object.soft_delete_restore` | ❌ | ✅ | ✅ | ✅ |
| `signed.v4_get` | ✅ | ✅ | ✅ | ✅ |
| `signed.v4_put` | ✅ | ✅ | ✅ | ✅ |
| `signed.v4_resumable_upload` | ❌ | ❌ | ❌ | ✅ |
| `signed.v4_response_overrides` | ✅ | ✅ | ❌ | ✅ |
| `signed.v2_get` | ✅ | ✅ | ✅ | ✅ |
| `signed.post_policy_v4` | ✅ | ❌ | ❌ | ✅ |
| `sdk.resumable_resume` | ❌ | ✅ | ✅ | ✅ |
| `sdk.resumable_cancel` | ❌ | ❌ | ❌ | ✅ |
| `sdk.object_update_replace` | ✅ | ✅ | ❌ | ✅ |
| `sdk.rewrite_token_loop` | ❌ | ✅ | ❌ | ✅ |
| `sdk.parallel_composite_upload` | ❌ | ✅ | ❌ | ✅ |
| `sdk.file_upload_download` | ✅ | ✅ | ✅ | ✅ |
