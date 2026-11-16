# floci-gcp 0.13.0 workflow checks

<!-- markdownlint-configure-file {"MD013": false} -->

Recorded on 2026-11-16 (UTC). These checks are the source of the floci-gcp column in the README's feature comparison. Each row applies the scope the README's table uses for the other products. A cell does not describe the whole API, and floci-gcp's many other Google Cloud services are outside this comparison.

## Inputs

| Input | Identity |
| --- | --- |
| Image | `floci/floci-gcp@sha256:a59403d43d524b1a08282d67d541a6d37bf7868cbd9d1311caa0d2983adf6f46` (tag `0.13.0`, native build) |
| Startup hook image | `floci/floci-gcp@sha256:8023144137113e19b6b799bbaabbe931d88e914141451a8373fc2eeea676ea2d` (tag `0.13.0-compat`) |
| Source | [floci-io/floci-gcp](https://github.com/floci-io/floci-gcp) tag `0.13.0`, revision `98829d47d8cf6ac7000eb2ed12f45259522339fd`, MIT |
| SDK results | [2026-11-16 scorecard](../../../sdk-scorecard/results/2026-11-16/summary.md) |

Requests went to a disposable container published on loopback, without a Docker socket. Unless a row says otherwise, it used the default `memory` storage and no other setting than `FLOCI_GCP_BASE_URL`.

## Results

| Workflow | Result | What was checked |
| --- | --- | --- |
| Create, list and delete multiple buckets | ✅ | Two buckets created and listed, one deleted (204) and gone from the list |
| Upload, download and delete live objects | ✅ | Upload 200, the same bytes back, delete 204, then 404 |
| Resumable upload: initiate, send chunks and finalize | ✅ | Two chunks of 256 KiB, 308 with `Range: bytes=0-262143`, then 200 and identical bytes |
| Copy live-object bytes | ✅ | `copyTo` 200 and identical bytes |
| Rewrite with multi-call continuation | ✅ | 5 MiB with `maxBytesRewrittenPerCall=1048576` to another storage class: four `done: false` answers with a `rewriteToken`, then `done: true` and identical bytes. A same-class rewrite finishes in one call by design, as its documentation says |
| Retain object version history | ✅ | Versioning on, two uploads, `versions=true` lists two generations, and the older generation returns its bytes |
| Keep API-created bytes across restart | ✅ Hybrid mode | `FLOCI_GCP_STORAGE_MODE=hybrid` on a named volume: after the 5-second flush and a container restart the object returned 200 with its bytes. The default `memory` mode keeps nothing |
| API writes visible as bucket/object files | ❌ | The volume holds one set of service files. Object bytes are in `gcs-object-data.json` and metadata in `gcs-objects.json`, not as bucket and object paths |
| Load bucket/object seed bytes | ◐ Startup hook script | There is no seed directory. A script in `/etc/floci-gcp/init/ready.d/` on the `0.13.0-compat` image created a bucket and copied a mounted file with `gcplocal`, and the object was then served |
| Default CORS for JSON API requests | ❌ | With no bucket rules, a preflight and a GET with `Origin` on the JSON API answered 200 without `Access-Control-Allow-Origin` |
| Enforce bucket-specific XML CORS rules | ✅ | With one allowed origin set on the bucket, XML GET and preflight returned the CORS headers for it and none for another origin |
| Upload/download with signed URLs | ✅ | `signed.v4_get` and `signed.v4_put` passed in all nine SDKs that the scorecard runs against floci-gcp. `signed.v4_resumable_upload` and `signed.post_policy_v4` did not pass in any of them |
| Verify V4 RSA signatures and expiry | ◐ Expiry only | A PUT and a GET with the made-up signature `00ff` answered 200. A URL whose date and lifetime had passed answered 400 `ExpiredToken`. Its documentation states that signatures are not cryptographically verified |
| Keep private buckets private and open public ones with `allUsers` | ✅ IAM enforce mode | With `FLOCI_GCP_SERVICES_IAM_AUTHORIZATION_MODE=enforce`, an object uploaded with a Floci-issued token answered 403 to anonymous JSON and XML reads and 200 after an `allUsers` `roles/storage.objectViewer` binding. The default mode does not enforce this, anonymous bucket creation stayed allowed, and its documentation says ACLs and gRPC data methods are not IAM-enforced |
| gRPC Storage v2 with the Go client | ◐ 5 of 8 cases | Bucket create/get/delete, write/read, list, update/delete and the failed precondition passed. `MultiRangeDownloader` answered `Unimplemented` for `BidiReadObject`, appendable objects are not supported, and a wrong CRC32C answered `DataLoss` instead of `InvalidArgument` |
| gRPC legacy Storage v1 reference RPCs | ❌ | `google.storage.v1.Storage/ListBuckets` and `GetBucket` answered gRPC status 12 (`Method not found`), while `google.storage.v2.Storage/GetBucket` reached the service and answered status 3 for its empty request |
| Published Docker image | ✅ Docker Hub | `floci/floci-gcp`, with `linux/amd64` and `linux/arm64` |

## SDK scorecard

floci-gcp scored 75.8 of 100 (Core 76.2), up from 74.2 (Core 74.6) for 0.12.0 on the same cases. 0.13.0 merges object metadata on JSON PATCH, so `object.update_metadata`, which already passed in Rust, now passes in every SDK, and Python's `sdk.json_batch` passes too. In the Go SDK the cases that did not pass were `bucket.metageneration_precondition`, `bucket.autoclass`, `object.checksum_rejected`, `object.copy_with_metadata`, `object.compose_preconditions`, `object.retention`, `object.csek_rotation`, `signed.v4_resumable_upload`, `signed.post_policy_v4` and the three gRPC cases above. The [summary](../../../sdk-scorecard/results/2026-11-16/summary.md) lists every case for every SDK.
