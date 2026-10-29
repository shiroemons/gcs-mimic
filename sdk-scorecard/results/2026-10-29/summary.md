# SDK scorecard

<!-- markdownlint-configure-file {"MD013": false, "MD024": {"siblings_only": true}} -->

Each SDK contributes 50 points; cases within an SDK have equal weight.

| Emulator | Version | Go | Ruby | Score (/100) |
| --- | --- | --- | --- | --- |
| fake-gcs-server | `fsouza/fake-gcs-server:1.56.1` | 24/57 | 24/51 | 44.6 |
| gcp-storage-emulator | `oittaa/gcp-storage-emulator:v2026.07.19` | 20/57 | 22/51 | 39.1 |
| gcs-mimic | `gcs-mimic 0.3.0 (7b81a6b)` | 45/57 | 40/51 | 78.7 |

## Passed cases by area

### Go

| Area | fake-gcs-server | gcp-storage-emulator | gcs-mimic |
| --- | --- | --- | --- |
| bucket | 5/20 | 7/20 | 14/20 |
| object | 16/27 | 10/27 | 23/27 |
| signed | 3/4 | 3/4 | 3/4 |
| grpc | 0/6 | 0/6 | 5/6 |

### Ruby

| Area | fake-gcs-server | gcp-storage-emulator | gcs-mimic |
| --- | --- | --- | --- |
| bucket | 5/20 | 8/20 | 14/20 |
| object | 15/27 | 11/27 | 23/27 |
| signed | 4/4 | 3/4 | 3/4 |

## Case results

### Go

| Case | fake-gcs-server | gcp-storage-emulator | gcs-mimic |
| --- | --- | --- | --- |
| `bucket.create_get_delete` | ✅ | ✅ | ✅ |
| `bucket.list_prefix_paging` | ✅ | ❌ | ✅ |
| `bucket.update_labels` | ❌ | ❌ | ✅ |
| `bucket.metageneration_precondition` | ❌ | ❌ | ✅ |
| `bucket.versioning_history` | ✅ | ❌ | ✅ |
| `bucket.lifecycle` | ❌ | ✅ | ✅ |
| `bucket.cors` | ❌ | ✅ | ✅ |
| `bucket.retention_policy` | ❌ | ❌ | ✅ |
| `bucket.retention_lock` | ❌ | ❌ | ✅ |
| `bucket.uniform_bucket_level_access` | ❌ | ✅ | ✅ |
| `bucket.public_access_prevention` | ❌ | ✅ | ✅ |
| `bucket.iam_policy` | ❌ | ✅ | ✅ |
| `bucket.iam_test_permissions` | ❌ | ✅ | ✅ |
| `bucket.soft_delete_policy` | ❌ | ❌ | ✅ |
| `bucket.autoclass` | ❌ | ❌ | ❌ |
| `bucket.acl` | ✅ | ❌ | ❌ |
| `bucket.default_object_acl` | ❌ | ❌ | ❌ |
| `bucket.notifications` | ✅ | ❌ | ❌ |
| `project.hmac_keys` | ❌ | ❌ | ❌ |
| `project.service_account` | ❌ | ❌ | ❌ |
| `object.write_read_small` | ✅ | ✅ | ✅ |
| `object.resumable_large` | ✅ | ❌ | ✅ |
| `object.metadata_on_write` | ✅ | ✅ | ✅ |
| `object.checksum_rejected` | ✅ | ✅ | ✅ |
| `object.md5_accepted` | ✅ | ✅ | ✅ |
| `object.range_reads` | ✅ | ❌ | ✅ |
| `object.update_metadata` | ❌ | ❌ | ✅ |
| `object.delete_not_found` | ✅ | ✅ | ✅ |
| `object.does_not_exist_precondition` | ✅ | ❌ | ✅ |
| `object.generation_match_precondition` | ❌ | ❌ | ✅ |
| `object.metageneration_match_precondition` | ❌ | ❌ | ✅ |
| `object.list_prefix_delimiter` | ✅ | ✅ | ✅ |
| `object.list_paging` | ✅ | ❌ | ✅ |
| `object.list_offsets` | ✅ | ❌ | ✅ |
| `object.list_match_glob` | ❌ | ✅ | ✅ |
| `object.copy_with_metadata` | ✅ | ❌ | ✅ |
| `object.rewrite_storage_class` | ❌ | ❌ | ✅ |
| `object.compose` | ❌ | ❌ | ✅ |
| `object.move` | ❌ | ❌ | ✅ |
| `object.acl` | ✅ | ❌ | ❌ |
| `object.holds` | ❌ | ❌ | ✅ |
| `object.retention` | ❌ | ❌ | ❌ |
| `object.custom_time` | ✅ | ✅ | ✅ |
| `object.storage_class_on_write` | ✅ | ✅ | ❌ |
| `object.gzip_transcoding` | ✅ | ❌ | ✅ |
| `object.customer_supplied_key` | ❌ | ❌ | ❌ |
| `object.soft_delete_restore` | ❌ | ✅ | ✅ |
| `signed.v4_get` | ✅ | ✅ | ✅ |
| `signed.v4_put` | ✅ | ✅ | ✅ |
| `signed.v2_get` | ✅ | ✅ | ✅ |
| `signed.post_policy_v4` | ❌ | ❌ | ❌ |
| `grpc.bucket_create_get_delete` | ❌ | ❌ | ✅ |
| `grpc.write_read` | ❌ | ❌ | ✅ |
| `grpc.list` | ❌ | ❌ | ✅ |
| `grpc.update_delete` | ❌ | ❌ | ✅ |
| `grpc.multi_range_downloader` | ❌ | ❌ | ✅ |
| `grpc.appendable_object` | ❌ | ❌ | ❌ |

### Ruby

| Case | fake-gcs-server | gcp-storage-emulator | gcs-mimic |
| --- | --- | --- | --- |
| `bucket.create_get_delete` | ✅ | ✅ | ✅ |
| `bucket.list_prefix_paging` | ✅ | ❌ | ✅ |
| `bucket.update_labels` | ❌ | ❌ | ✅ |
| `bucket.metageneration_precondition` | ❌ | ❌ | ✅ |
| `bucket.versioning_history` | ✅ | ❌ | ✅ |
| `bucket.lifecycle` | ❌ | ✅ | ✅ |
| `bucket.cors` | ❌ | ✅ | ✅ |
| `bucket.retention_policy` | ❌ | ❌ | ✅ |
| `bucket.retention_lock` | ❌ | ❌ | ✅ |
| `bucket.uniform_bucket_level_access` | ❌ | ✅ | ✅ |
| `bucket.public_access_prevention` | ❌ | ✅ | ✅ |
| `bucket.iam_policy` | ❌ | ✅ | ✅ |
| `bucket.iam_test_permissions` | ❌ | ✅ | ✅ |
| `bucket.soft_delete_policy` | ❌ | ✅ | ✅ |
| `bucket.autoclass` | ❌ | ❌ | ❌ |
| `bucket.acl` | ✅ | ❌ | ❌ |
| `bucket.default_object_acl` | ❌ | ❌ | ❌ |
| `bucket.notifications` | ✅ | ❌ | ❌ |
| `project.hmac_keys` | ❌ | ❌ | ❌ |
| `project.service_account` | ❌ | ❌ | ❌ |
| `object.write_read_small` | ✅ | ✅ | ✅ |
| `object.resumable_large` | ✅ | ✅ | ✅ |
| `object.metadata_on_write` | ❌ | ✅ | ✅ |
| `object.checksum_rejected` | ✅ | ✅ | ✅ |
| `object.md5_accepted` | ✅ | ✅ | ✅ |
| `object.range_reads` | ✅ | ❌ | ✅ |
| `object.update_metadata` | ❌ | ❌ | ✅ |
| `object.delete_not_found` | ✅ | ✅ | ✅ |
| `object.does_not_exist_precondition` | ✅ | ❌ | ✅ |
| `object.generation_match_precondition` | ❌ | ❌ | ✅ |
| `object.metageneration_match_precondition` | ❌ | ❌ | ✅ |
| `object.list_prefix_delimiter` | ✅ | ✅ | ✅ |
| `object.list_paging` | ✅ | ❌ | ✅ |
| `object.list_offsets` | ✅ | ❌ | ✅ |
| `object.list_match_glob` | ❌ | ✅ | ✅ |
| `object.copy_with_metadata` | ✅ | ❌ | ✅ |
| `object.rewrite_storage_class` | ❌ | ❌ | ✅ |
| `object.compose` | ❌ | ❌ | ✅ |
| `object.move` | ❌ | ❌ | ✅ |
| `object.acl` | ✅ | ❌ | ❌ |
| `object.holds` | ❌ | ❌ | ✅ |
| `object.retention` | ❌ | ❌ | ❌ |
| `object.custom_time` | ✅ | ✅ | ✅ |
| `object.storage_class_on_write` | ✅ | ✅ | ❌ |
| `object.gzip_transcoding` | ✅ | ❌ | ✅ |
| `object.customer_supplied_key` | ❌ | ❌ | ❌ |
| `object.soft_delete_restore` | ❌ | ✅ | ✅ |
| `signed.v4_get` | ✅ | ✅ | ✅ |
| `signed.v4_put` | ✅ | ✅ | ✅ |
| `signed.v2_get` | ✅ | ✅ | ✅ |
| `signed.post_policy_v4` | ✅ | ❌ | ❌ |
