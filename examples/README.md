# Examples

Each directory runs the same workflow against gcs-mimic with an official client: create a bucket, upload, list, download and delete an object.

## Start gcs-mimic

Start the emulator once and run any example against it:

```sh
docker run -d --name gcs-mimic -p 127.0.0.1:4443:4443 -p 127.0.0.1:4444:4444 -e GCS_MIMIC_SCHEME=http -e GCS_MIMIC_GRPC_LISTEN=0.0.0.0:4444 ghcr.io/shiroemons/gcs-mimic:0.10.0
```

Only the Rust example needs the gRPC listener on port 4444; the others use HTTP on port 4443 alone.

## Clients

| Example | Client and version | Note |
| --- | --- | --- |
| [go](go/) | `cloud.google.com/go/storage` v1.70.0 | Connects through `STORAGE_EMULATOR_HOST`. |
| [ruby](ruby/) | `google-cloud-storage` 1.62.1 | Uses an anonymous client with a local project. |
| [node](node/) | `@google-cloud/storage` 8.4.0 | Uses `apiEndpoint`, not `STORAGE_EMULATOR_HOST`. |
| [python](python/) | `google-cloud-storage` 3.17.1 | Runs with uv, which installs Python. |
| [php](php/) | `google/cloud-storage` 2.6.1 | Runs with local PHP and Composer, or only Docker. |
| [java](java/) | `google-cloud-storage` 2.76.0 | Uses `HttpStorageOptions` with `NoCredentials`. |
| [dotnet](dotnet/) | `Google.Cloud.Storage.V1` 5.0.0 | Uses `UnauthenticatedAccess` with a custom `BaseUri`. |
| [rust](rust/) | `google-cloud-storage` 1.21.0 | Objects over HTTP on 4443, buckets over gRPC on 4444. |
| [gcloud](gcloud/) | `gcloud storage` (Google Cloud SDK 591.0.0) | Also covers resumable, parallel composite and sliced transfers. |

Each directory's README lists its requirements. Run an example with `./run.sh` (`./gcloud-example.sh` for gcloud); set `GCS_MIMIC_ENDPOINT` to use an address other than `http://localhost:4443`.
