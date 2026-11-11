# Official .NET HTTP SDK and Testcontainers

`global.json` pins .NET SDK 11.0.100. `Scenario.csproj` pins Storage 5.0.0 and
Testcontainers 4.16.0; `packages.lock.json` records the complete graph.
The project targets `net10.0` and `net11.0`, so the suite keeps proving the LTS
runtime while it also runs on the newest one. `run.sh` runs every mode, including
`--container`, once per target framework, prints `== .NET SDK suite on <framework>`
and the runtime each run used, and fails if either run fails or runs on another
major runtime. SDK 11 installs only the .NET 11 runtime; the .NET 10 runtime comes
from the exact LTS SDK in `.dotnet-lts-sdk-version` (10.0.404, runtime 10.0.15),
which CI installs beside the `global.json` SDK. Locally, install that SDK too.
`bash scripts/test-dotnet-frameworks.sh` checks the per-framework runs with a
stub `dotnet`.

```sh
bash sdk-tests/dotnet/run.sh --self-check
GCSM_ENDPOINT=http://127.0.0.1:4443 bash sdk-tests/dotnet/run.sh
GCSM_IMAGE=gcs-mimic:dev bash sdk-tests/dotnet/run.sh --container
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
