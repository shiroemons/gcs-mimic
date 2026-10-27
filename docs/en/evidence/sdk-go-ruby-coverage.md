# Go and Ruby SDK coverage survey: 2026-10-27

<!-- markdownlint-configure-file {"MD013": false} -->

This survey records which official Go and Ruby Storage SDK calls work against
gcs-mimic at `main` `461bdd9`. It combines the pinned SDK suites with a one-off
probe that called SDK methods outside those suites. The probe is evidence for
this snapshot only; it does not change `compatibility/features.json` statuses.
Japanese companion: [SDK coverage (ja)](../../ja/sdk-go-ruby-coverage.md).

| Input | Version |
| --- | --- |
| Go | 1.27.3 |
| `cloud.google.com/go/storage` | v1.70.0 |
| Ruby / Bundler | 4.0.9 / 4.0.23 |
| `google-cloud-storage` (Ruby) | 1.62.1 |

## Method

1. `just check-sdk`: the pinned Go suite against permissive and strict HTTP
   servers, then the Ruby self-check, migration scenario, permissive run and
   strict run with V4 signatures.
2. Probe: one permissive HTTP server (`serve --scheme http`) with a fresh data
   directory. The Go probe used the default client (JSON API, XML reads) plus a
   `WithJSONReads` client; every call had a 20-second deadline. The Ruby probe
   used `Google::Cloud::Storage.anonymous` with `retries: 0`. Each call was
   classified by its outcome; successful settings were read back to detect
   accepted-but-ignored values.

## Results

| Run | Result |
| --- | --- |
| `just check-sdk` | PASS (exit 0): Go vet, 53 top-level tests and 89 subtests without failures or skips; Ruby self-check, migration scenario, permissive and strict runs including V4 signed URLs |
| Go probe | 87 calls: 49 work, 21 return 501 (8 of them never return without a deadline), 16 return 404, 1 gRPC connection failure |
| Ruby probe | 101 calls: 56 work, 30 return 501, 15 return 404 |
| Accepted-but-ignored values | None found in either SDK |

### Legend

| Mark | Meaning |
| --- | --- |
| ✅ | Works; values read back as written, or the expected error is returned |
| 501 | Explicit `notImplemented` rejection |
| 501⏳ | 501, but the Go SDK retries 5xx without an attempt limit, so the call does not return until the caller's deadline |
| 404 | Route not implemented; the SDK reports "not found", indistinguishable from a missing resource |
| — | Not applicable or not probed for that SDK |

### Buckets

| Operation | Go | Ruby | Notes |
| --- | --- | --- | --- |
| Create with defaults | ✅ | ✅ | Ruby default `autoclass:{enabled:false}` is accepted |
| Create with versioning, labels, CORS | ✅ | ✅ | Go probed versioning at creation; labels and CORS through update |
| Create in a regional location (`ASIA-NORTHEAST1`) | 501⏳ | 501 | Historical probe result; [regional creation now passes](../slices/sdk-regional-locations.md) in both SDK suites, with predefined dual-region creation additionally covered by Go |
| Create with predefined ACL / default object ACL | 501⏳ | 501 | |
| Create with uniform bucket-level access | 501⏳ | — | Ruby update also returns 501 |
| Create with hierarchical namespace | 501⏳ | — | |
| Create with website, requester pays, autoclass, object retention | 501⏳ (object retention) | 501 | |
| Duplicate create returns 409 | ✅ | ✅ | |
| Get, missing bucket, `exists?` | ✅ | ✅ | |
| List (project, prefix) | ✅ | ✅ | |
| Update labels, versioning, CORS, public access prevention, storage class, default event-based hold | ✅ | ✅ | |
| Update lifecycle | 501 | 501 | |
| Update retention policy | 501 | 501 | |
| Lock retention policy | 501⏳ | 501 | Go call depends on a retention policy, which fails first |
| Update uniform bucket-level access | 501 | 501 | |
| Update website, logging, requester pays | 501 | 501 | |
| Update soft delete policy, autoclass, default KMS key | 501 | 501 | |
| Update RPO | — | 501 | |
| Stale metageneration precondition returns 412 | ✅ | ✅ | |
| IAM get, set, testIamPermissions | ✅ | ✅ | |
| Bucket ACL list, add, delete | 404 | 404 | Ruby `acl.public!` returns 501 |
| Default object ACL list, add | 404 | 404 | |
| Notifications create, list, delete | 404 | 404 | |
| Delete empty bucket; non-empty returns 409 | ✅ | ✅ | |
| Restore soft-deleted bucket | — | 404 | |

### Project resources

| Operation | Go | Ruby |
| --- | --- | --- |
| Service account email | 404 | 404 |
| HMAC key create, list | 404 | 404 |

### Objects

| Operation | Go | Ruby | Notes |
| --- | --- | --- | --- |
| Simple upload | ✅ | ✅ | Ruby `StringIO` without `content_type` works |
| Resumable upload (3 chunks of 256 KiB) | ✅ | ✅ | |
| Content headers and custom metadata | ✅ | ✅ | |
| Checksum mismatch rejected | ✅ | ✅ | Go CRC32C, Ruby MD5 |
| `ifGenerationMatch=0` on an existing object returns 412 | ✅ | ✅ | |
| Upload with predefined ACL | 501 | 501 | |
| Upload with KMS key | 501 | 501 | |
| Customer-supplied encryption key (write, read, rotate) | 501 | 501 | |
| Download (Go XML default and JSON reads; Ruby to `StringIO` and path) | ✅ | ✅ | |
| Range download | ✅ | ✅ | |
| Missing object reports not found | ✅ | ✅ | |
| Gzip transcoding and compressed reads | ✅ | ✅ | |
| Get metadata | ✅ | ✅ | |
| Update metadata, content type | ✅ | ✅ | |
| Temporary and event-based holds | ✅ | ✅ | |
| Custom time | ✅ | ✅ | |
| Change storage class through rewrite | — | ✅ | |
| Object retention | 501⏳ | 501 | Go fails when creating the retention-enabled bucket |
| Stale metageneration precondition returns 412 | ✅ | ✅ | |
| Copy, including across buckets with overrides | ✅ | ✅ | |
| Rewrite | ✅ | ✅ | Go is covered by the suite |
| Compose | ✅ | ✅ | |
| Move | 404 | 404 | |
| Object ACL list, add | 404 | 404 | Ruby `acl.public!` returns 501 |
| Delete; missing object reports not found | ✅ | ✅ | |
| Read a noncurrent generation, list versions, delete a generation | ✅ | ✅ | |
| Restore a soft-deleted object | 404 | 404 | |
| List soft-deleted objects | 501⏳ | 501 | |

### Listing

| Option | Go | Ruby | Notes |
| --- | --- | --- | --- |
| Prefix, delimiter, paging | ✅ | ✅ | |
| Match glob | ✅ | ✅ | |
| Start and end offsets | ✅ | — | Ruby 1.62.1 has no offset arguments |
| Include trailing delimiter, field selection | ✅ | — | Not probed in Ruby |
| Include folders as prefixes | 501⏳ | 501 | |

### Signing, authentication and transport

| Area | Go | Ruby | Notes |
| --- | --- | --- | --- |
| V4 signed URL GET/PUT and response overrides (strict) | ✅ | ✅ | Suite. Go v1.70.0 drops non-default ports from the canonical host; Ruby needs `bucket_bound_hostname` |
| POST policy V4 form upload | 501 | 501 | Ruby V2 `post_object` also returns 501 |
| Strict authentication, viewer/creator/admin roles | ✅ | ✅ | Suite |
| HTTPS with an explicit CA | ✅ | — | Go suite; the Ruby HTTPS path was not run by `just check-sdk` |
| gRPC client | Not available | — | No gRPC listener; the Go gRPC client fails with `Unavailable` |
| `public_url` | — | Points to `storage.googleapis.com` | SDK behavior |

## Observations

- The Go SDK retries 408, 429 and every 5xx status, including 501, without an
  attempt limit by default. Idempotent calls that reach a 501 (bucket creation,
  retention lock, list options) therefore block until the caller's context
  deadline. Ruby with default retries fails after about 9 seconds.
- Unimplemented JSON routes (ACLs, notifications, HMAC keys, service account,
  move, restore) return 404 `Not Found`. SDKs surface this as a missing bucket
  or object rather than an unsupported operation.
- No probe found a setting that was accepted and then ignored.

## Not performed

No real-cloud comparison, HTTPS Ruby rerun, Rails Active Storage run, other
SDKs, benchmark or resource measurement was performed. The probe sources were
one-off scripts and are not committed.
