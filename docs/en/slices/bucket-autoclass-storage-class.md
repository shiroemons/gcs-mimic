# Bucket Autoclass and object storage class on write

<!-- markdownlint-configure-file {"MD013": false} -->

Scope: BEHAVIOR-BUCKET-019 (Autoclass configuration) and the object-creation part
of BEHAVIOR-BUCKET-004 (storage classes). BEHAVIOR-BUCKET-020 (access-driven
Autoclass transitions) stays NOT_STARTED.

## Sources

Checked 2026-10-29 against the [Autoclass page](https://docs.cloud.google.com/storage/docs/autoclass),
the [bucket resource](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets),
the pinned Discovery bucket schema in `compatibility/upstream/json-schemas.json`
and the pinned `google.storage.v2.Bucket.Autoclass` message.

- `autoclass.enabled` and `autoclass.terminalStorageClass` are writable;
  `toggleTime` and `terminalStorageClassUpdateTime` are output only.
- Valid terminal classes are NEARLINE and ARCHIVE; the default is NEARLINE.
- `toggleTime` is the last time Autoclass was enabled or disabled; the v2 proto
  sets it to the bucket create time when Autoclass is enabled at creation.
- While enabled, the bucket default class is always STANDARD and requests that
  change it to another class fail. New objects start in STANDARD even when the
  request names another class, and rewrite class changes are ignored.
- Autoclass cannot coexist with lifecycle `SetStorageClass` actions or
  `matchesStorageClass` conditions.
- Enabling Autoclass moves every object except soft-deleted ones to STANDARD.
- Autoclass cannot be re-enabled until one day after it was disabled.

## Contract and acceptance

| Case | Required behavior | Tests |
| --- | --- | --- |
| AUTOCLASS-CREATE | JSON insert and gRPC CreateBucket with `enabled:true` store Autoclass with server-computed `toggleTime` and `terminalStorageClassUpdateTime` equal to the create time and default NEARLINE; ARCHIVE is accepted; supplied JSON output-only times are ignored and gRPC output-only times are rejected. GET, read masks and restart read it back. | `bucket_autoclass_create_get_patch_and_restrictions`, `grpc_buckets_create_autoclass_shares_json_rules`, `autoclass_schema_migration_preserves_buckets_and_persists_configuration` |
| AUTOCLASS-VALIDATION | Invalid terminal classes, non-boolean `enabled`, unknown Autoclass properties, a terminal class while disabled, a non-STANDARD bucket class and lifecycle SetStorageClass/matchesStorageClass rules with Autoclass return 400 invalidArgument without creating or changing the bucket. Delete-only lifecycle rules coexist. | `bucket_autoclass_create_get_patch_and_restrictions`, `autoclass_json_parsing_validates_fields`, `autoclass_bucket_restrictions`, `ruby_default_create_bucket` |
| AUTOCLASS-UPDATE | PATCH, PUT and gRPC UpdateBucket (`autoclass`, `autoclass.enabled`, `autoclass.terminal_storage_class`) merge Autoclass: changing the terminal class sets its update time and keeps `toggleTime`; disabling (`enabled:false` or `autoclass:null`) records a new `toggleTime` and removes the terminal class; re-enabling within 24 hours of disabling fails; omitted Autoclass keeps the current configuration. | `bucket_autoclass_create_get_patch_and_restrictions`, `grpc_update_bucket_autoclass_masks`, `autoclass_enable_sets_server_times_and_default_terminal_class`, `autoclass_disable_and_reenable_boundary` |
| AUTOCLASS-ENABLE-EXISTING | Enabling Autoclass on an existing bucket sets its default class to STANDARD and moves its non-soft-deleted objects to STANDARD in the same catalog transaction without changing object metageneration; an explicit non-STANDARD class in the same request fails without mutation. | `bucket_autoclass_enable_moves_bucket_and_objects_to_standard` |
| OBJECT-CLASS-ON-WRITE | JSON multipart and resumable `storageClass`, XML PUT `x-goog-storage-class` and gRPC `WriteObjectSpec.resource.storage_class` store STANDARD, NEARLINE, COLDLINE or ARCHIVE and read it back; null or omitted uses the bucket default. Unknown, lowercase, empty and non-string classes return 400; REGIONAL/RAPID keep the existing unsupported error. Resumable sessions persist the requested class. | `object_storage_class_on_write_json_xml_and_autoclass`, `obj_multipart_http_rejects_bad_metadata_and_unsupported_fields_without_mutation`, `xml_put_unsupported_inputs_fail_before_continue_and_preserve_state`, `grpc_io_write_spec_storage_class` |
| OBJECT-CLASS-AUTOCLASS | In an enabled Autoclass bucket, uploads (all adapters), copy, rewrite and compose store STANDARD regardless of the requested class; rewrite class changes outside Autoclass still apply; object PATCH keeps treating storageClass as output only. | `object_storage_class_on_write_json_xml_and_autoclass` |
| SDK-SCORECARD | Go and Ruby `bucket.autoclass` and `object.storage_class_on_write` scorecard probes pass. | `sdk-scorecard/run.sh` |

## Local decisions and limits

- Autoclass transitions driven by access or age (BEHAVIOR-BUCKET-020) are not
  simulated: objects in an enabled bucket stay STANDARD, and changing the
  terminal class moves no objects. Pricing, the 128 KiB rule and relocation
  pauses are not modeled.
- The error status and reason for restriction violations and early re-enabling
  are not documented; locally they are 400 invalidArgument.
- PUT preserves an omitted `autoclass`, matching the local PUT rule for other
  fields and Ruby's `Bucket#update`, which sends only changed attributes.
- Ruby's default `{"enabled":false}`, `{}`, `{"enabled":null}` and `null` on a
  bucket that never enabled Autoclass leave it unconfigured, so responses do not
  advertise Autoclass (SPEC-RUBY-DEFAULTS).
- The server computes Autoclass times when it applies the change; they fall
  between the previous and new bucket `updated` times but are not guaranteed to
  equal the new `updated`.
- Catalog schema 31 adds a nullable `buckets.autoclass` JSON column; older
  catalogs migrate without changing existing buckets.
