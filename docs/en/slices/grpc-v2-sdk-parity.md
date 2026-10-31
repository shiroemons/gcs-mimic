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
