# SDK scorecard

<!-- markdownlint-configure-file {"MD013": false, "MD024": {"siblings_only": true}} -->

Each SDK contributes 12.5 points; cases within an SDK have equal weight.
Cases the SDK has no public API for are shown as n/a and excluded from that SDK's denominator.
Score counts every case, including each SDK's own idioms (sdk area) and transports (grpc area).
Core counts only the bucket/object/signed cases shared by every SDK, for cross-SDK comparison.

| Emulator | Version | Go | Ruby | Node.js | PHP | Python | Java | .NET | Rust | Score (/100) | Core (/100) |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| fake-gcs-server | `fsouza/fake-gcs-server:1.56.1` | 24/57 | 24/51 | 25/51 | 25/51 | 19/51 | 35/70 | 31/70 (1 n/a) | 0/47 (4 n/a) | 39.8 | 39.8 |
| floci-gcp | `floci/floci-gcp:0.12.0` | 48/57 | 42/51 | 44/51 | 42/51 | 39/51 | 52/70 | 51/70 (1 n/a) | 29/47 (4 n/a) | 77.6 | 78.5 |
| gcp-storage-emulator | `oittaa/gcp-storage-emulator:v2026.07.19` | 20/57 | 22/51 | 22/51 | 20/51 | 24/51 | 35/70 | 31/70 (1 n/a) | 0/47 (4 n/a) | 37.7 | 38.8 |
| gcs-mimic | `gcs-mimic 0.16.0 (b0338de7)` | 57/57 | 51/51 | 51/51 | 51/51 | 51/51 | 70/70 | 70/70 (1 n/a) | 47/47 (4 n/a) | 100 | 100 |

## Passed cases by area

### Go

| Area | fake-gcs-server | floci-gcp | gcp-storage-emulator | gcs-mimic |
| --- | --- | --- | --- | --- |
| bucket | 5/20 | 18/20 | 7/20 | 20/20 |
| object | 16/27 | 23/27 | 10/27 | 27/27 |
| signed | 3/4 | 3/4 | 3/4 | 4/4 |
| grpc | 0/6 | 4/6 | 0/6 | 6/6 |

### Ruby

| Area | fake-gcs-server | floci-gcp | gcp-storage-emulator | gcs-mimic |
| --- | --- | --- | --- | --- |
| bucket | 5/20 | 16/20 | 8/20 | 20/20 |
| object | 15/27 | 23/27 | 11/27 | 27/27 |
| signed | 4/4 | 3/4 | 3/4 | 4/4 |

### Node.js

| Area | fake-gcs-server | floci-gcp | gcp-storage-emulator | gcs-mimic |
| --- | --- | --- | --- | --- |
| bucket | 5/20 | 18/20 | 8/20 | 20/20 |
| object | 16/27 | 23/27 | 11/27 | 27/27 |
| signed | 4/4 | 3/4 | 3/4 | 4/4 |

### PHP

| Area | fake-gcs-server | floci-gcp | gcp-storage-emulator | gcs-mimic |
| --- | --- | --- | --- | --- |
| bucket | 5/20 | 17/20 | 8/20 | 20/20 |
| object | 16/27 | 22/27 | 9/27 | 27/27 |
| signed | 4/4 | 3/4 | 3/4 | 4/4 |

### Python

| Area | fake-gcs-server | floci-gcp | gcp-storage-emulator | gcs-mimic |
| --- | --- | --- | --- | --- |
| bucket | 3/20 | 15/20 | 11/20 | 20/20 |
| object | 12/27 | 21/27 | 10/27 | 27/27 |
| signed | 4/4 | 3/4 | 3/4 | 4/4 |

### Java

| Area | fake-gcs-server | floci-gcp | gcp-storage-emulator | gcs-mimic |
| --- | --- | --- | --- | --- |
| bucket | 4/20 | 13/20 | 10/20 | 20/20 |
| object | 16/27 | 22/27 | 12/27 | 27/27 |
| signed | 4/4 | 3/4 | 3/4 | 4/4 |
| sdk | 11/19 | 14/19 | 10/19 | 19/19 |

### .NET

| Area | fake-gcs-server | floci-gcp | gcp-storage-emulator | gcs-mimic |
| --- | --- | --- | --- | --- |
| bucket | 4/20 | 16/20 | 10/20 | 20/20 |
| object | 14/26 | 20/26 | 12/26 | 26/26 |
| signed | 3/4 | 3/4 | 3/4 | 4/4 |
| sdk | 10/20 | 12/20 | 6/20 | 20/20 |

### Rust

| Area | fake-gcs-server | floci-gcp | gcp-storage-emulator | gcs-mimic |
| --- | --- | --- | --- | --- |
| bucket | 0/17 | 8/17 | 0/17 | 17/17 |
| object | 0/27 | 19/27 | 0/27 | 27/27 |
| signed | 0/3 | 2/3 | 0/3 | 3/3 |

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
| `object.update_metadata` | ❌ | ❌ | ❌ | ✅ |
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
| `object.move` | ❌ | ✅ | ❌ | ✅ |
| `object.acl` | ✅ | ✅ | ❌ | ✅ |
| `object.holds` | ❌ | ✅ | ❌ | ✅ |
| `object.retention` | ❌ | ❌ | ❌ | ✅ |
| `object.custom_time` | ✅ | ✅ | ✅ | ✅ |
| `object.storage_class_on_write` | ✅ | ✅ | ✅ | ✅ |
| `object.gzip_transcoding` | ✅ | ✅ | ❌ | ✅ |
| `object.customer_supplied_key` | ❌ | ✅ | ❌ | ✅ |
| `object.soft_delete_restore` | ❌ | ✅ | ✅ | ✅ |
| `signed.v4_get` | ✅ | ✅ | ✅ | ✅ |
| `signed.v4_put` | ✅ | ✅ | ✅ | ✅ |
| `signed.v2_get` | ✅ | ✅ | ✅ | ✅ |
| `signed.post_policy_v4` | ❌ | ❌ | ❌ | ✅ |
| `grpc.bucket_create_get_delete` | ❌ | ✅ | ❌ | ✅ |
| `grpc.write_read` | ❌ | ✅ | ❌ | ✅ |
| `grpc.list` | ❌ | ✅ | ❌ | ✅ |
| `grpc.update_delete` | ❌ | ✅ | ❌ | ✅ |
| `grpc.multi_range_downloader` | ❌ | ❌ | ❌ | ✅ |
| `grpc.appendable_object` | ❌ | ❌ | ❌ | ✅ |

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
| `object.update_metadata` | ❌ | ❌ | ❌ | ✅ |
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
| `object.move` | ❌ | ✅ | ❌ | ✅ |
| `object.acl` | ✅ | ✅ | ❌ | ✅ |
| `object.holds` | ❌ | ✅ | ❌ | ✅ |
| `object.retention` | ❌ | ❌ | ❌ | ✅ |
| `object.custom_time` | ✅ | ✅ | ✅ | ✅ |
| `object.storage_class_on_write` | ✅ | ✅ | ✅ | ✅ |
| `object.gzip_transcoding` | ✅ | ✅ | ❌ | ✅ |
| `object.customer_supplied_key` | ❌ | ✅ | ❌ | ✅ |
| `object.soft_delete_restore` | ❌ | ✅ | ✅ | ✅ |
| `signed.v4_get` | ✅ | ✅ | ✅ | ✅ |
| `signed.v4_put` | ✅ | ✅ | ✅ | ✅ |
| `signed.v2_get` | ✅ | ✅ | ✅ | ✅ |
| `signed.post_policy_v4` | ✅ | ❌ | ❌ | ✅ |

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
| `object.update_metadata` | ❌ | ❌ | ❌ | ✅ |
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
| `object.move` | ❌ | ✅ | ❌ | ✅ |
| `object.acl` | ✅ | ✅ | ❌ | ✅ |
| `object.holds` | ❌ | ✅ | ❌ | ✅ |
| `object.retention` | ❌ | ❌ | ❌ | ✅ |
| `object.custom_time` | ✅ | ✅ | ✅ | ✅ |
| `object.storage_class_on_write` | ✅ | ✅ | ✅ | ✅ |
| `object.gzip_transcoding` | ✅ | ✅ | ❌ | ✅ |
| `object.customer_supplied_key` | ❌ | ✅ | ❌ | ✅ |
| `object.soft_delete_restore` | ❌ | ✅ | ✅ | ✅ |
| `signed.v4_get` | ✅ | ✅ | ✅ | ✅ |
| `signed.v4_put` | ✅ | ✅ | ✅ | ✅ |
| `signed.v2_get` | ✅ | ✅ | ✅ | ✅ |
| `signed.post_policy_v4` | ✅ | ❌ | ❌ | ✅ |

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
| `object.update_metadata` | ❌ | ❌ | ❌ | ✅ |
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
| `object.move` | ❌ | ✅ | ❌ | ✅ |
| `object.acl` | ✅ | ✅ | ❌ | ✅ |
| `object.holds` | ❌ | ❌ | ❌ | ✅ |
| `object.retention` | ❌ | ❌ | ❌ | ✅ |
| `object.custom_time` | ✅ | ✅ | ✅ | ✅ |
| `object.storage_class_on_write` | ✅ | ✅ | ✅ | ✅ |
| `object.gzip_transcoding` | ✅ | ✅ | ❌ | ✅ |
| `object.customer_supplied_key` | ❌ | ✅ | ❌ | ✅ |
| `object.soft_delete_restore` | ❌ | ✅ | ✅ | ✅ |
| `signed.v4_get` | ✅ | ✅ | ✅ | ✅ |
| `signed.v4_put` | ✅ | ✅ | ✅ | ✅ |
| `signed.v2_get` | ✅ | ✅ | ✅ | ✅ |
| `signed.post_policy_v4` | ✅ | ❌ | ❌ | ✅ |

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
| `object.update_metadata` | ❌ | ❌ | ❌ | ✅ |
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
| `object.move` | ❌ | ✅ | ❌ | ✅ |
| `object.acl` | ✅ | ❌ | ✅ | ✅ |
| `object.holds` | ❌ | ❌ | ❌ | ✅ |
| `object.retention` | ❌ | ❌ | ❌ | ✅ |
| `object.custom_time` | ✅ | ✅ | ✅ | ✅ |
| `object.storage_class_on_write` | ✅ | ✅ | ✅ | ✅ |
| `object.gzip_transcoding` | ❌ | ✅ | ❌ | ✅ |
| `object.customer_supplied_key` | ❌ | ✅ | ❌ | ✅ |
| `object.soft_delete_restore` | ❌ | ✅ | ✅ | ✅ |
| `signed.v4_get` | ✅ | ✅ | ✅ | ✅ |
| `signed.v4_put` | ✅ | ✅ | ✅ | ✅ |
| `signed.v2_get` | ✅ | ✅ | ✅ | ✅ |
| `signed.post_policy_v4` | ✅ | ❌ | ❌ | ✅ |

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
| `object.update_metadata` | ❌ | ❌ | ❌ | ✅ |
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
| `object.move` | ❌ | ✅ | ❌ | ✅ |
| `object.acl` | ✅ | ❌ | ✅ | ✅ |
| `object.holds` | ❌ | ✅ | ❌ | ✅ |
| `object.retention` | ❌ | ❌ | ❌ | ✅ |
| `object.custom_time` | ✅ | ✅ | ✅ | ✅ |
| `object.storage_class_on_write` | ✅ | ✅ | ✅ | ✅ |
| `object.gzip_transcoding` | ✅ | ✅ | ❌ | ✅ |
| `object.customer_supplied_key` | ❌ | ✅ | ❌ | ✅ |
| `object.soft_delete_restore` | ❌ | ✅ | ✅ | ✅ |
| `signed.v4_get` | ✅ | ✅ | ✅ | ✅ |
| `signed.v4_put` | ✅ | ✅ | ✅ | ✅ |
| `signed.v2_get` | ✅ | ✅ | ✅ | ✅ |
| `signed.post_policy_v4` | ✅ | ❌ | ❌ | ✅ |
| `sdk.blob_handle_roundtrip` | ✅ | ✅ | ✅ | ✅ |
| `sdk.default_upload_crc32c` | ✅ | ❌ | ✅ | ✅ |
| `sdk.create_from_path_download_to` | ✅ | ✅ | ✅ | ✅ |
| `sdk.write_channel_streaming` | ✅ | ✅ | ✅ | ✅ |
| `sdk.read_channel_chunked` | ✅ | ✅ | ✅ | ✅ |
| `sdk.list_iterate_all` | ❌ | ✅ | ❌ | ✅ |
| `sdk.storage_batch` | ✅ | ✅ | ❌ | ✅ |
| `sdk.default_retry_resumable_recovery` | ✅ | ✅ | ✅ | ✅ |
| `sdk.blob_write_session_default` | ✅ | ✅ | ✅ | ✅ |
| `sdk.transfer_manager_upload` | ✅ | ✅ | ✅ | ✅ |
| `sdk.transfer_manager_download` | ✅ | ✅ | ✅ | ✅ |
| `sdk.transfer_manager_parallel_composite` | ❌ | ❌ | ❌ | ✅ |
| `sdk.write_session_parallel_composite` | ❌ | ❌ | ❌ | ✅ |
| `sdk.write_session_buffer_to_disk` | ✅ | ✅ | ✅ | ✅ |
| `sdk.grpc_basic_object` | ❌ | ✅ | ❌ | ✅ |
| `sdk.grpc_blob_read_session` | ❌ | ❌ | ❌ | ✅ |
| `sdk.grpc_write_session_bidi` | ❌ | ✅ | ❌ | ✅ |
| `sdk.grpc_write_session_journaling` | ❌ | ✅ | ❌ | ✅ |
| `sdk.grpc_appendable_upload` | ❌ | ❌ | ❌ | ✅ |

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
| `object.update_metadata` | ❌ | ❌ | ❌ | ✅ |
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
| `object.move` | ❌ | ✅ | ❌ | ✅ |
| `object.acl` | ✅ | ❌ | ✅ | ✅ |
| `object.holds` | ❌ | ✅ | ❌ | ✅ |
| `object.retention` | ❌ | ❌ | ❌ | ✅ |
| `object.custom_time` | ✅ | ✅ | ✅ | ✅ |
| `object.storage_class_on_write` | ✅ | ✅ | ✅ | ✅ |
| `object.gzip_transcoding` | n/a | n/a | n/a | n/a |
| `object.customer_supplied_key` | ❌ | ✅ | ❌ | ✅ |
| `object.soft_delete_restore` | ❌ | ✅ | ✅ | ✅ |
| `signed.v4_get` | ✅ | ✅ | ✅ | ✅ |
| `signed.v4_put` | ✅ | ✅ | ✅ | ✅ |
| `signed.v2_get` | ✅ | ✅ | ✅ | ✅ |
| `signed.post_policy_v4` | ❌ | ❌ | ❌ | ✅ |
| `sdk.download_hash_validation` | ✅ | ✅ | ✅ | ✅ |
| `sdk.download_chunked` | ✅ | ✅ | ✅ | ✅ |
| `sdk.update_object_implicit_preconditions` | ❌ | ❌ | ❌ | ✅ |
| `sdk.update_bucket_etag_ignored` | ❌ | ❌ | ❌ | ✅ |
| `sdk.iam_policy_etag` | ❌ | ❌ | ❌ | ✅ |
| `sdk.resumable_resume_from_uri` | ❌ | ✅ | ✅ | ✅ |
| `sdk.object_uploader_session` | ✅ | ✅ | ✅ | ✅ |
| `sdk.upload_progress` | ✅ | ✅ | ✅ | ✅ |
| `sdk.download_gzip_default` | ✅ | ✅ | ❌ | ✅ |
| `sdk.list_auto_paging` | ✅ | ✅ | ❌ | ✅ |
| `sdk.conditional_replace_and_read` | ❌ | ✅ | ❌ | ✅ |
| `sdk.delete_bucket_with_objects` | ✅ | ✅ | ✅ | ✅ |
| `sdk.list_fields_partial` | ❌ | ❌ | ❌ | ✅ |
| `sdk.signed_url_resumable_upload` | ❌ | ❌ | ❌ | ✅ |
| `sdk.signed_request_template_headers` | ✅ | ✅ | ❌ | ✅ |
| `sdk.signed_query_generation` | ✅ | ✅ | ❌ | ✅ |
| `sdk.service_rewrite_token_loop` | ❌ | ✅ | ❌ | ✅ |
| `sdk.csek_rotation` | ❌ | ❌ | ❌ | ✅ |
| `sdk.bucket_soft_delete_restore` | ❌ | ❌ | ❌ | ✅ |
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
| `object.move` | ❌ | ❌ | ❌ | ✅ |
| `object.acl` | ❌ | ❌ | ❌ | ✅ |
| `object.holds` | ❌ | ✅ | ❌ | ✅ |
| `object.retention` | ❌ | ❌ | ❌ | ✅ |
| `object.custom_time` | ❌ | ✅ | ❌ | ✅ |
| `object.storage_class_on_write` | ❌ | ✅ | ❌ | ✅ |
| `object.gzip_transcoding` | ❌ | ✅ | ❌ | ✅ |
| `object.customer_supplied_key` | ❌ | ✅ | ❌ | ✅ |
| `object.soft_delete_restore` | ❌ | ❌ | ❌ | ✅ |
| `signed.v4_get` | ❌ | ✅ | ❌ | ✅ |
| `signed.v4_put` | ❌ | ✅ | ❌ | ✅ |
| `signed.v2_get` | n/a | n/a | n/a | n/a |
| `signed.post_policy_v4` | ❌ | ❌ | ❌ | ✅ |
