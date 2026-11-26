# Roadmap

[日本語](../ja/roadmap.md)

Updated 2026-11-26. The order below selects work; it does not promise dates.

## Goal

gcs-mimic is a local Cloud Storage emulator for development and tests. The
official Cloud Storage SDKs, and tools built on them such as Rails Active
Storage and `gcloud storage`, should work against it with nothing changed but
the endpoint setting.

These are not goals:

- answering byte for byte as Cloud Storage does;
- covering every Google Cloud Storage API and management service;
- becoming a complete superset of fake-gcs-server.

Where official sources leave behavior open, gcs-mimic picks what keeps the
SDKs working and records it as local behavior. Compatibility is judged by the
official SDKs, Google documentation and the googleapis protos; nothing in the
repository contacts Cloud Storage
([why](development.md#no-live-cloud-storage-contact)).

## Scope

This is the product profile that `NOT_APPLICABLE` rows in the
[feature ledger](../../compatibility/features.json) refer to. The ledger owns
every status; the [feature matrix](feature-matrix.md) shows them.

In scope:

- What the official SDKs call over the JSON API: bucket and object CRUD,
  media, multipart and resumable uploads, downloads and ranges, listing,
  copy, rewrite, compose and move, metadata and preconditions, versions,
  holds, retention and soft delete.
- Storage v2 gRPC as the SDKs' gRPC clients use it, and the legacy Storage v1
  RPCs already served.
- The XML API that SDKs and browsers use: signed URLs, signed POST policies,
  XML multipart upload and Rails Active Storage direct uploads.
- The access control SDK workflows meet: IAM policies, ACLs, uniform
  bucket-level access, public access prevention, HMAC keys and local
  identities in strict mode.
- Running the emulator: configuration, bucket initialization, seed and import,
  migration from fake-gcs-server, native archives and container images.

Out of scope:

- Storage Control v2, folders, managed folders and hierarchical namespace.
- Storage Insights, Storage Intelligence and Storage Batch Operations.
- Anywhere Cache, Rapid Cache and the Cloud Storage MCP server.
- Long-running operations, locations and FeatureConfigs.
- XML bucket configuration, ACL, retention, encryption and HMAC
  subresources, XML multi-object delete, XML compose and HTML form upload.
- Object contexts, bucket relocation, bulk restore, object IAM, website
  routing, usage logging, IP filters, encryption enforcement, RPO and Autoclass
  transitions.
- Google infrastructure and other services: KMS, OAuth token issuance, VPC
  Service Controls, CDN, billing, Monitoring and Logging, Storage Transfer
  Service, Firebase and Cloud Storage FUSE.
- Benchmark matrices, performance comparisons with fake-gcs-server, and
  resource budgets beyond local-development scale.

A row that is already `PARTIAL` keeps its code, status and pending cases. An
out-of-scope capability returns as a new slice when an official SDK workflow
needs it.

## Next

1. **Keep the SDKs working.** The
   [nine-SDK scorecard](../../sdk-scorecard/README.md) and the SDK suites pass
   on every release. Add the SDK workflows that users report, and make the
   suites prove that they never leave the machine (DESIGN-088).
2. **Close gaps that SDK workflows reach.** Finish a `PARTIAL` row's pending
   cases when an SDK workflow needs them, not to complete the row for its own
   sake.
3. **Keep releases shippable.** Every release passes the distribution and
   dependency gates.

## 1.0.0

1.0.0 means that the nine scorecard SDKs, at their pinned versions, pass every
scored case on the release commit, and that upgrades within `1.x` keep
documented integrations and data directories working. It does not mean that
every `PARTIAL` row is complete. The full conditions, including a seven-day
release-candidate soak, are in the
[release policy](release-policy.md#the-100-gate).
