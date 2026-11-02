# Storage v2 bucket, ACL and IAM parity for official SDKs

<!-- markdownlint-configure-file {"MD013": false} -->

Scope: gRPC differences that the official Java SDK 2.76.0 and Rust SDK (`google-cloud-storage` 1.21.0) probes found in bucket creation, bucket metadata, ACLs, IAM, object retention and soft-deleted listings. The Rust SDK performs every bucket, metadata, ACL, IAM, listing and copy operation through gRPC (its `StorageControl` client), so these differences block Rust users completely. Each behavior is a separate change; the acceptance cases below were recorded before the corresponding fix.

The contract is the [pinned Storage v2 proto](https://github.com/googleapis/googleapis/blob/933dd1be8180a22bca2dc473b53d4f4297ee3b0c/google/storage/v2/storage.proto) recorded in the [foundation contract](grpc-v2-foundation.md). Where the proto text and Cloud Storage behavior differ, an observation against Cloud Storage on 2026-10-31 (gRPC `storage.googleapis.com:443`, a client built from the Go SDK's generated `storagepb`) decides the scope. All affected rows remain PARTIAL.

## CreateBucket `bucket.name`

The proto says that populating `bucket.name` is an error, but the Java SDK sends `CreateBucket{parent: "projects/_", bucket_id: X, bucket: {name: "projects/_/buckets/X", project: "projects/<p>"}}`, and Cloud Storage created the bucket for that request.

| Case | Required behavior |
| --- | --- |
| grpc_bucket_create_named | `bucket.name` equal to `projects/_/buckets/<bucket_id>` creates the bucket named by `bucket_id`. |
| grpc_bucket_create_name_mismatch | Any other nonempty `bucket.name` (a different bucket, a bare ID, another project segment or extra path segments) returns INVALID_ARGUMENT and creates nothing. |
| grpc_bucket_create_name_authorization | Strict project authorization is unchanged; the name check adds no permission. |

A mismatching name is rejected rather than ignored: either value could be the intended bucket, and creating the wrong one is not reversible from the caller's view. INVALID_ARGUMENT keeps the proto's documented error for a populated name; Cloud Storage's response to a mismatching name was not observed.

Test: `grpc_buckets_create_accepts_name_matching_bucket_id` failed on the previous source with INVALID_ARGUMENT for the matching name and passes after the change; `grpc_buckets_strict_shared_authorization` still passes.

## Stored bucket fields in gRPC responses

A lifecycle configured through JSON was missing from GetBucket: only UpdateBucket and LockBucketRetentionPolicy responses converted it. An audit of `pb::Bucket` against the stored bucket record found two more dropped fields, `etag` and the `object_retention` read-mask path.

| Case | Required behavior |
| --- | --- |
| grpc_bucket_lifecycle | One shared conversion renders stored lifecycle rules (action, storage class and every condition) in GetBucket, ListBuckets, CreateBucket, UpdateBucket and LockBucketRetentionPolicy responses. A bucket without rules has no `lifecycle` message. |
| grpc_bucket_etag | `etag` carries the same bucket ETag as the JSON resource. |
| grpc_bucket_read_mask | `etag`, `lifecycle` and `object_retention` can be selected by read_mask; other selected fields stay empty. |

The remaining `pb::Bucket` fields are either covered by other changes in this slice (`acl`, `default_object_acl`, `owner`) or not modeled by the bucket record (`rpo`, `website`, `logging`, `encryption`, `billing`, `custom_placement_config`, `hierarchical_namespace`, `ip_filter`, `satisfies_pzs`); they stay empty and their read-mask paths remain UNIMPLEMENTED.

Test: `grpc_buckets_expose_json_lifecycle_etag_and_object_retention` failed on the previous source (no lifecycle in GetBucket) and passes after the change.

## Bucket IAM through Storage Control

The Rust SDK sends bucket IAM calls only to `google.storage.control.v2.StorageControl/{GetIamPolicy,SetIamPolicy,TestIamPermissions}`, which returned UNIMPLEMENTED ("unknown service or method"). The [pinned Storage Control proto](https://github.com/googleapis/googleapis/blob/933dd1be8180a22bca2dc473b53d4f4297ee3b0c/google/storage/control/v2/storage_control.proto) (SHA-256 `9a054e7de962aa895130fe18d4a25dd79486fb2d418ba295a785ddd85c9be735`, retrieved 2026-10-31) declares these methods with the same `google.iam.v1` request and response messages as Storage v2. The dispatcher therefore serves the three Storage Control paths with the existing Storage v2 handlers; no Storage Control protobuf closure is generated and every other Storage Control method stays UNIMPLEMENTED.

| Case | Required behavior |
| --- | --- |
| control_bucket_iam_get | GetIamPolicy on `projects/_/buckets/<bucket>` reads the shared policy store and returns its opaque ETag. |
| control_bucket_iam_set | SetIamPolicy with the current ETag replaces the bindings and returns a new ETag; GetIamPolicy then returns the written policy. |
| control_bucket_iam_stale_etag | SetIamPolicy with a stale ETag returns FAILED_PRECONDITION and leaves the policy unchanged. |
| control_bucket_iam_test | TestIamPermissions returns exactly the requested permissions that the caller holds. |
| control_bucket_iam_strict | Strict profile: no credentials return UNAUTHENTICATED; a registered caller without `storage.buckets.getIamPolicy`/`setIamPolicy` receives PERMISSION_DENIED; TestIamPermissions returns an empty list for that caller. |
| control_child_resource_iam | Managed-folder and object resources are valid IAM resources without a local policy store and return UNIMPLEMENTED with ErrorInfo on both services, not INVALID_ARGUMENT. |
| control_other_methods | Other Storage Control methods remain UNIMPLEMENTED. |

Cloud Storage returned HTTP 412 for a stale `setIamPolicy` ETag over JSON (observed 2026-10-31). The gRPC status was not observed. The emulator keeps its shared mapping of HTTP 412 to FAILED_PRECONDITION, which Storage v2 already uses for generation and metageneration precondition failures; the generic `google.rpc.Code` guidance would also allow ABORTED for a concurrency conflict, so the exact provider status remains an open question.

Test: `grpc_storage_control_bucket_iam` sends encoded calls through the same dispatch function as a connection. With the Storage Control paths disabled, its first call failed with UNIMPLEMENTED; it passes after the change.

## Bucket and object ACLs

The Rust SDK exposes ACLs only as resource fields: it reads `Bucket.acl`, appends an entry and writes the whole list with UpdateBucket (`acl` or `default_object_acl` mask) or UpdateObject (`acl` mask). These masks returned UNIMPLEMENTED, and GetBucket returned no ACL. The pinned proto defines GetBucket's default read mask as all fields and ListBuckets' default as all fields except `acl`, `default_object_acl` and `owner`.

| Case | Required behavior |
| --- | --- |
| grpc_bucket_get_acl | GetBucket without a mask, with `*`, or with `acl`, `default_object_acl` or `owner` returns the stored ACLs (the `projectPrivate` defaults for a new bucket) and the owner, rendered by the JSON ACL code so ids, etags and entity fields match JSON `projection=full`. ListBuckets returns them only when the mask selects them. Create, Update and LockBucketRetentionPolicy responses include them. |
| grpc_bucket_acl_omitted | Under uniform bucket-level access the ACLs are omitted and the owner remains, as in JSON. A strict caller without `storage.buckets.getIamPolicy` still reads the bucket, without ACL fields. |
| grpc_bucket_acl_masks | `acl` and `default_object_acl` replace the stored list through the shared JSON entry validation and catalog transaction; only `entity` and `role` are written. JSON then observes the new entries. |
| grpc_object_acl_mask | UpdateObject `acl` replaces the object ACL through the same shared path, advances the metageneration and returns the ACL. |
| grpc_acl_errors | An invalid entity, or any ACL write under uniform bucket-level access, returns INVALID_ARGUMENT (the shared JSON 400) and changes nothing. |
| grpc_acl_strict | A strict caller without `storage.buckets.update` receives PERMISSION_DENIED and an anonymous caller UNAUTHENTICATED; the ACL is unchanged. The catalog's shared ACL permission checks apply as in JSON. |

GetObject's ACL projection and `predefined_acl` on create, update and compose remain outside this change and keep their current behavior; `acl.<field>` and `owner` update masks return UNIMPLEMENTED.

Tests: `grpc_buckets_acl_read_and_update`, `grpc_buckets_acl_strict_authorization` and `grpc_update_object_acl` failed on the previous source (empty bucket ACL; UNIMPLEMENTED masks) and pass after the change. The negative mask test now uses the still-unsupported `acl.entity` and `owner` paths instead of `acl`.

## Object retention

CreateBucket `enable_object_retention` and the UpdateObject `retention` mask and `override_unlocked_retention` flag returned UNIMPLEMENTED. They now use the JSON insert rule, the shared retention parser, the shared transition check and the shared strict permission checks.

| Case | Required behavior |
| --- | --- |
| grpc_bucket_create_object_retention | `enable_object_retention=true` creates a bucket with object retention enabled (`object_retention.enabled` in the response). In strict mode the caller also needs `storage.buckets.enableObjectRetention` on the project; without it the call returns PERMISSION_DENIED and creates nothing. |
| grpc_bucket_object_retention_field | `bucket.object_retention.enabled=true` without the request flag returns INVALID_ARGUMENT, as JSON rejects `objectRetention` without `enableObjectRetention=true`; with the flag it is accepted. |
| grpc_object_retention_set | The `retention` mask with a mode and `retain_until_time` stores the configuration, returns it and protects the object from deletion. |
| grpc_object_retention_override | Shortening or removing (an absent `retention` message) an Unlocked configuration fails without `override_unlocked_retention` and leaves it unchanged; with the flag it succeeds. |
| grpc_object_retention_disabled | A bucket without object retention returns INVALID_ARGUMENT (the shared JSON 400). |
| grpc_object_retention_strict | A strict caller without `storage.objects.setRetention` receives PERMISSION_DENIED and the object keeps no retention; `override_unlocked_retention` additionally requires `storage.objects.overrideUnlockedRetention`. |

An unspecified mode, or a mode without `retain_until_time`, returns INVALID_ARGUMENT. `retention.<field>` masks remain UNIMPLEMENTED. Writes that set retention at upload (WriteObject resource fields) are outside this change.

Tests: `grpc_buckets_create_enables_object_retention`, `grpc_update_object_retention` and `grpc_update_object_retention_strict` failed on the previous source with UNIMPLEMENTED and pass after the change. The negative mask test now uses the still-unsupported `retention.mode` path instead of `retention`.

## Soft-deleted listings

ListObjects `soft_deleted=true` returned UNIMPLEMENTED. The adapter already builds a JSON listing query from the request, so it now forwards `softDeleted` to the shared JSON listing.

| Case | Required behavior |
| --- | --- |
| grpc_list_soft_deleted | `soft_deleted=true` returns only soft-deleted generations, with their generation, `soft_delete_time` and `hard_delete_time`; an ordinary listing omits them. |
| grpc_list_soft_deleted_versions | Combining `soft_deleted` with `versions` returns INVALID_ARGUMENT, as JSON rejects `softDeleted=true&versions=true`. |
| grpc_list_soft_deleted_strict | Strict mode uses the same `storage.objects.list` check as JSON: a caller with the permission lists, a registered caller without it receives PERMISSION_DENIED and an anonymous caller UNAUTHENTICATED. |

Prefix, delimiter, offsets, glob and page tokens combine with `soft_deleted` exactly as in the JSON listing. `filter` remains UNIMPLEMENTED; `include_folders_as_prefixes` follows the JSON rule of the [storage layout slice](bucket-storage-layout.md); GetObject `soft_deleted` is outside this change.

Tests: `grpc_list_soft_deleted_objects` and `grpc_list_soft_deleted_objects_strict` failed on the previous source with UNIMPLEMENTED and pass after the change.
