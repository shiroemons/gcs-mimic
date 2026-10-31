# Official .NET HTTP SDK and Testcontainers

`global.json` pins .NET SDK 10.0.403. `Scenario.csproj` pins Storage 5.0.0 and
Testcontainers 4.16.0; `packages.lock.json` records the complete graph.

```sh
bash sdk-tests/dotnet/run.sh --self-check
GCS_MIMIC_ENDPOINT=http://127.0.0.1:4443 bash sdk-tests/dotnet/run.sh
GCS_MIMIC_IMAGE=gcs-mimic:dev bash sdk-tests/dotnet/run.sh --container
```

The builder sets `UnauthenticatedAccess=true` and an explicit BaseUri including
the JSON API's `/storage/v1/` path. A message
handler checks every request and absolute Location against the selected loopback
HTTP origin; ambient proxies and automatic redirects are disabled so a redirect
cannot bypass the guard. The SDK's normal gzip/decompression and explicit
resumable Location handling are retained. HTTPS, strict credentials,
gRPC and remote Docker are separate scopes.

The container adapter uses a random mapped port, the healthcheck alias and
opt-in advertised-endpoint update before any resumable initiation. Each run
uses a unique bucket, verifies three resumable chunks, exact bytes, metadata,
generation, listing and a failed create-if-absent overwrite, then deletes its
fixtures and disposes only its own container.

The [SDK adapter contract](../../docs/en/slices/sdk-client-adapters.md) records
the difference between compile/self-check and final runtime/container evidence.
