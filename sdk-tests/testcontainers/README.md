# Minimal Testcontainers adapters

These adapters use generic containers, preserving the normal image entrypoint.
They avoid wrappers hard-coded to `fsouza/fake-gcs-server` or to the executable
name `fake-gcs-server`. Set `GCS_MIMIC_IMAGE` to the exact local or digest-pinned
image under test; no published future image is assumed.

```sh
cd sdk-tests/testcontainers/node
npm ci --ignore-scripts --no-audit --no-fund
node scenario.mjs --self-check
GCS_MIMIC_IMAGE=gcs-mimic:dev node scenario.mjs
```

Java and .NET adapters are in `sdk-tests/java` and `sdk-tests/dotnet`; run their
`run.sh --container` entry points. Each adapter exposes container port 4443 on
a random host port, waits for `/_internal/healthcheck`, then updates
`externalUrl`/`publicHost` through opt-in management before resumable initiation.
Unique bucket names, no mounted shared volume and finally/disposal teardown
keep parallel invocations isolated. The SDK phase checks only the selected
loopback HTTP origin; remote Docker hosts and HTTPS are outside this scope.

Container startup/image pulls communicate with Docker before the SDK origin
guard is installed; Docker lifecycle is not a Cloud Storage auth request.
All three adapters pass actual startup-to-resumable execution and cleanup
against the same final ARM64 source image. The pinned Node SDK's two listener
warnings remain visible. [Combined evidence](../../docs/en/evidence/migration-integration.md)
records the exact image identity, commands, results and remaining profile limits.
