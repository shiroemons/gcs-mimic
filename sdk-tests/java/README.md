# Official Java HTTP SDK and Testcontainers

Use Java 17 or newer and Maven 3.10.0. Compilation/self-check verification used
Temurin 27.0.1.0.2+1. Exact SDK/plugin pins live in `pom.xml`; `dependencies.lock`
records the resolved graph and `run.sh` fails if that graph changes.
After compilation, the pinned Exec plugin runs a separate JVM with Maven's
resolved `%classpath`. This gives Testcontainers' process-lifetime daemons
normal JVM shutdown instead of Maven's in-process thread cleanup; see the
[official Exec example](https://www.mojohaus.org/exec-maven-plugin/examples/example-exec-for-java-programs.html).
Container disposal, Ryuk and nonzero child-exit propagation remain enabled.

```sh
bash sdk-tests/java/run.sh --self-check
GCS_MIMIC_ENDPOINT=http://127.0.0.1:4443 bash sdk-tests/java/run.sh
GCS_MIMIC_IMAGE=gcs-mimic:dev bash sdk-tests/java/run.sh --container
```

The HTTP client uses anonymous credentials and checks every URL, including SDK
redirects and upload sessions, before creating a connection. Proxies and
automatic connection redirects are disabled. Only the selected loopback HTTP
origin is allowed. HTTPS, gRPC and remote Docker hosts are separate scopes.

The container adapter uses the image's normal `gcs-mimic` entrypoint, a random
mapped port and the healthcheck alias. After readiness it advertises the actual
mapped endpoint through the opt-in management API. It then creates a unique
bucket and checks multipart upload, metadata PATCH/method override, 576 KiB
resumable upload, bytes, generation and listing. It cleans the bucket in finally
and closes its own container without a shared host port or volume.

The [SDK adapter contract](../../docs/en/slices/sdk-client-adapters.md) separates
successful compile/self-check evidence from final runtime/container acceptance.
