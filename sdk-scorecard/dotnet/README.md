# .NET SDK scorecard probe

<!-- markdownlint-configure-file {"MD013": false} -->

Probes the official `Google.Cloud.Storage.V1` 5.0.0 against an emulator. It
emits one JSONL line per case: the 51 shared bucket, object and signed cases
(same IDs and order as the Python probe) plus 20 .NET-specific cases in the
`sdk` area (IDs start with `sdk.`): progress reporting, resumable sessions,
signed-URL templates, rewrite loops and similar idioms.

## Build and run

Needs the exact .NET SDK in `global.json` (roll-forward is disabled).

```sh
bash sdk-scorecard/dotnet/run.sh build   # restore with --locked-mode and compile
SCORECARD_ENDPOINT=https://127.0.0.1:PORT SCORECARD_GRPC_ENDPOINT=127.0.0.1:GRPC_PORT \
  SCORECARD_CA_CERT=ca.pem SCORECARD_SIGNING_KEY=fixtures/signing/test-only-private.pem \
  bash sdk-scorecard/dotnet/run.sh --no-build   # JSONL on stdout
```

`--transport-check` only verifies CA, hostname and origin handling.

## Unsupported cases

Cases the SDK has no public API for are emitted with `unsupported: true` and
excluded from the .NET denominator.

| Case | Reason |
| --- | --- |
| `object.gzip_transcoding` | 5.0.0 always decompresses downloads; `StorageClientBuilder.GZipEnabled` is internal, so a gzip-encoded object cannot be fetched without decompression. |

## Bucket update and If-Match

`UpdateBucketAsync` sends the fetched ETag as `If-Match` and no
`ifMetagenerationMatch`. Cloud Storage ignores `If-Match` on bucket PUT and
PATCH (observed 2026-10-31), so `sdk.update_bucket_etag_ignored` expects a
stale update to succeed, unlike the object update which returns 412.
