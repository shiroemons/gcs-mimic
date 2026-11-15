# SDK scorecard

<!-- markdownlint-configure-file {"MD013": false, "MD024": {"siblings_only": true}} -->

Each SDK contributes 100/9 (about 11.1) points; cases within an SDK have equal weight.
Cases the SDK has no public API for are shown as n/a and excluded from that SDK's denominator.
Score counts every case, including each SDK's own idioms (sdk area) and transports (grpc area).
Core counts only the bucket/object/signed cases shared by every SDK, for cross-SDK comparison.

| Emulator | Version | Go | Ruby | Node.js | PHP | Python | Java | .NET | Rust | C++ | Score (/100) | Core (/100) |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| fake-gcs-server | `fsouza/fake-gcs-server:1.56.1` | 25/61 (1 n/a) | 23/55 (1 n/a) | 26/56 | 26/56 | 20/56 | 36/74 (1 n/a) | 32/72 (1 n/a) | 0/51 (5 n/a) | 25/55 (1 n/a) | 38.9 | 38.5 |
| floci-gcp | `floci/floci-gcp:0.12.0` | 49/61 (1 n/a) | 42/55 (1 n/a) | 45/56 | 43/56 | 40/56 | 53/74 (1 n/a) | 52/72 (1 n/a) | 31/51 (5 n/a) | 44/55 (1 n/a) | 74.4 | 74.6 |
| gcp-storage-emulator | `oittaa/gcp-storage-emulator:v2026.07.19` | 20/61 (1 n/a) | 22/55 (1 n/a) | 22/56 | 20/56 | 24/56 | 35/74 (1 n/a) | 31/72 (1 n/a) | 0/51 (5 n/a) | 23/55 (1 n/a) | 35.9 | 36.3 |
| gcs-mimic | `gcs-mimic 0.22.0 (43d84560)` | 61/61 (1 n/a) | 55/55 (1 n/a) | 56/56 | 56/56 | 56/56 | 74/74 (1 n/a) | 72/72 (1 n/a) | 50/51 (5 n/a) | 55/55 (1 n/a) | 99.8 | 99.8 |

## Passed cases by area

### Go

| Area | fake-gcs-server | floci-gcp | gcp-storage-emulator | gcs-mimic |
| --- | --- | --- | --- | --- |
| bucket | 5/20 | 18/20 | 7/20 | 20/20 |
| object | 16/29 | 23/29 | 10/29 | 29/29 |
| signed | 4/6 | 4/6 | 3/6 | 6/6 |
| grpc | 0/6 | 4/6 | 0/6 | 6/6 |

### Ruby

| Area | fake-gcs-server | floci-gcp | gcp-storage-emulator | gcs-mimic |
| --- | --- | --- | --- | --- |
| bucket | 5/21 | 16/21 | 8/21 | 21/21 |
| object | 14/28 | 22/28 | 11/28 | 28/28 |
| signed | 4/6 | 4/6 | 3/6 | 6/6 |

### Node.js

| Area | fake-gcs-server | floci-gcp | gcp-storage-emulator | gcs-mimic |
| --- | --- | --- | --- | --- |
| bucket | 5/21 | 18/21 | 8/21 | 21/21 |
| object | 16/29 | 23/29 | 11/29 | 29/29 |
| signed | 5/6 | 4/6 | 3/6 | 6/6 |

### PHP

| Area | fake-gcs-server | floci-gcp | gcp-storage-emulator | gcs-mimic |
| --- | --- | --- | --- | --- |
| bucket | 5/21 | 17/21 | 8/21 | 21/21 |
| object | 16/29 | 22/29 | 9/29 | 29/29 |
| signed | 5/6 | 4/6 | 3/6 | 6/6 |

### Python

| Area | fake-gcs-server | floci-gcp | gcp-storage-emulator | gcs-mimic |
| --- | --- | --- | --- | --- |
| bucket | 3/21 | 15/21 | 11/21 | 21/21 |
| object | 12/29 | 21/29 | 10/29 | 29/29 |
| signed | 5/6 | 4/6 | 3/6 | 6/6 |

### Java

| Area | fake-gcs-server | floci-gcp | gcp-storage-emulator | gcs-mimic |
| --- | --- | --- | --- | --- |
| bucket | 4/20 | 13/20 | 10/20 | 20/20 |
| object | 16/29 | 22/29 | 12/29 | 29/29 |
| signed | 5/6 | 4/6 | 3/6 | 6/6 |
| sdk | 11/19 | 14/19 | 10/19 | 19/19 |

### .NET

| Area | fake-gcs-server | floci-gcp | gcp-storage-emulator | gcs-mimic |
| --- | --- | --- | --- | --- |
| bucket | 4/21 | 16/21 | 10/21 | 21/21 |
| object | 14/28 | 20/28 | 12/28 | 28/28 |
| signed | 4/6 | 4/6 | 3/6 | 6/6 |
| sdk | 10/17 | 12/17 | 6/17 | 17/17 |

### Rust

| Area | fake-gcs-server | floci-gcp | gcp-storage-emulator | gcs-mimic |
| --- | --- | --- | --- | --- |
| bucket | 0/17 | 8/17 | 0/17 | 17/17 |
| object | 0/29 | 20/29 | 0/29 | 28/29 |
| signed | 0/5 | 3/5 | 0/5 | 5/5 |

### C++

| Area | fake-gcs-server | floci-gcp | gcp-storage-emulator | gcs-mimic |
| --- | --- | --- | --- | --- |
| bucket | 5/20 | 18/20 | 9/20 | 20/20 |
| object | 15/29 | 22/29 | 11/29 | 29/29 |
| signed | 5/6 | 4/6 | 3/6 | 6/6 |

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
| `object.update_metadata` | ❌ | ❌ | ❌ | ✅ |
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
| `sdk.resumable_resume_from_uri` | ❌ | ✅ | ✅ | ✅ |
| `sdk.object_uploader_session` | ✅ | ✅ | ✅ | ✅ |
| `sdk.upload_progress` | ✅ | ✅ | ✅ | ✅ |
| `sdk.download_gzip_default` | ✅ | ✅ | ❌ | ✅ |
| `sdk.list_auto_paging` | ✅ | ✅ | ❌ | ✅ |
| `sdk.conditional_replace_and_read` | ❌ | ✅ | ❌ | ✅ |
| `sdk.delete_bucket_with_objects` | ✅ | ✅ | ✅ | ✅ |
| `sdk.list_fields_partial` | ❌ | ❌ | ❌ | ✅ |
| `sdk.signed_request_template_headers` | ✅ | ✅ | ❌ | ✅ |
| `sdk.signed_query_generation` | ✅ | ✅ | ❌ | ✅ |
| `sdk.service_rewrite_token_loop` | ❌ | ✅ | ❌ | ✅ |
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
| `object.csek_rotation` | ❌ | ❌ | ❌ | ❌ |
| `object.soft_delete_restore` | ❌ | ❌ | ❌ | ✅ |
| `signed.v4_get` | ❌ | ✅ | ❌ | ✅ |
| `signed.v4_put` | ❌ | ✅ | ❌ | ✅ |
| `signed.v4_resumable_upload` | ❌ | ❌ | ❌ | ✅ |
| `signed.v4_response_overrides` | ❌ | ✅ | ❌ | ✅ |
| `signed.v2_get` | n/a | n/a | n/a | n/a |
| `signed.post_policy_v4` | ❌ | ❌ | ❌ | ✅ |

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
