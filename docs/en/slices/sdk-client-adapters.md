# Additional SDK and container migration adapters

<!-- markdownlint-configure-file {"MD013": false} -->

This provides independently compiled client harnesses for AUDIT-001 (Java),
AUDIT-002 (.NET), DESIGN-087 (official Rust), the migration gate's Testcontainers
clients, and explicit Node adapters under DESIGN-085. The feature ledger owns
status. Compile/self-check success is not successful SDK or container execution.
Existing five-SDK scorecard results remain unchanged.

## Acceptance boundary

| Client | Selected final runtime acceptance | Verified evidence |
| --- | --- | --- |
| Java HTTP Storage | Create unique bucket, ordinary multipart upload with SDK gzip defaults, PATCH through the official method-override transport, three-chunk 576 KiB resumable transfer, exact read, generation, prefix listing, cleanup | Native HTTP scenario passes with the exact dependency graph and unchanged SDK payloads. |
| .NET HTTP Storage | Unique bucket, three-chunk resumable transfer, exact read, metadata update, generation/listing, failed create-if-absent overwrite and cleanup | Native HTTP scenario passes with default gzip and returned-ETag If-Match enabled; [the metadata condition is a local extension](json-metadata-if-match.md). |
| Official Rust Storage | Unique bucket through official gRPC control, forced HTTP resumable write, generation/metadata, chunk-by-chunk exact read and cleanup | Native scenario passes through the explicit local frontdoor; HTTP 5, gRPC 4, blocked 0. |
| Arrow object_store | XML PUT/GET/range/list, copy-if-absent and duplicate-copy failure, cleanup | Independent native scenario passes; HTTP 6, gRPC 5, blocked 0. This is third-party evidence. |
| Node/Java/.NET Testcontainers | Generic image startup, readiness, random mapped port, advertised endpoint update, real SDK resumable transfer and finally/disposal teardown | All three actual scenarios and cleanup pass on the same final ARM64 image. |
| Node strict auth/lifecycle adapters | Public authenticated initiation followed by streamed upload to the same-origin capability; lifecycle replacement without an undeclared query | Four unit cases cover ordered initiation, unchanged conditions, cross-origin rejection, lifecycle wire object and literal signer arguments |
| Node V4 adapter | Sign the final host:port with the local Rust signer; strict PUT/GET success and method/Host tampering rejection | Syntax and argument checks; the existing upstream-SDK nondefault-port rejection test is retained |

Every SDK selects a local endpoint and explicit anonymous or static local
credentials. Java checks each connection through its transport factory, with
native redirects/proxies disabled; .NET checks each message and absolute
Location with automatic redirects disabled. Node's SDK phase enforces
the destination at `Socket.connect`, restoring that guard before Docker teardown.
These checks reject the metadata server and other hosts/ports. Setup downloads
and Docker image management are separate from SDK authentication.

The official Rust SDK has no public custom HTTP transport setter in the pinned
release, and its builder creates both HTTP and gRPC transports for one endpoint.
`go/cmd/sdk-guard --grpc-endpoint` therefore provides an explicit test-only
HTTP/1+h2c frontdoor, with distinct fixed loopback JSON and gRPC backends.
The official client selects that frontdoor; HTTP Reqwest also honors the checked
proxy environment, and Arrow receives the proxy explicitly. Only the frontdoor
and selected HTTP backend are accepted as HTTP origins; relative h2c requests
must match the frontdoor authority and use the fixed gRPC backend. Eight active
requests and bounded copy buffers limit forwarding. CONNECT, foreign origins
and metadata destinations are rejected, and the wrapper fails on guard bypass.
Eleven Go guard tests plus two subcases pass with the race detector. This is a
client-harness boundary, not an OS network sandbox or a combined product
listener. The product's JSON and plaintext gRPC listeners remain separate.

All container adapters keep the normal image entrypoint and use HTTP
`/_internal/healthcheck` readiness. After obtaining the actual mapped port,
they update `externalUrl`/`publicHost` with administration explicitly enabled,
before initiating resumable uploads. Random ports, UUID buckets and disposable
container storage provide parallel isolation; no fixed host port or shared
developer volume is used. Their final runtime run is deliberately deferred to
the final integrated source rather than repeated during edits.

## Node and PHP migration recipes

Node Storage 8.4.0's streaming upload path omits
`useAuthWithCustomEndpoint` during initiation. Use its authenticated public
method before streaming, without disabling strict server authentication:

```js
import {uploadWithLocalAuth} from './sdk-tests/node/migration-adapters.mjs';
await uploadWithLocalAuth(bucket, 'fixture.bin', {
  origin: new URL('http://127.0.0.1:4443'), destination: 'dir/fixture.bin',
  chunkSize: 256 * 1024, preconditionOpts: {ifGenerationMatch: 0},
});
```

The adapter rejects a session whose origin differs. Chunks use the capability
URI, as described by Google's [resumable upload contract](https://docs.cloud.google.com/storage/docs/performing-resumable-uploads).
It preserves the SDK's chunking, checksum verification, conditions and streaming.

The same Node release signs V4 canonical Host with `hostname`, dropping a
nondefault port while retaining that port in the emitted URL. The existing
negative SDK test remains evidence of that upstream limitation. For a strict
local URL with a nondefault port, use the explicit offline CLI adapter:

```js
import {signV4WithCli} from './sdk-tests/node/migration-adapters.mjs';
const url = await signV4WithCli(new URL('http://127.0.0.1:4443'), {
  bucket: 'private-bucket', object: 'dir/file.txt', method: 'PUT',
  signer: 'local-admin@example.invalid', privateKey: 'test-only-private.pem',
  headers: {'content-type': 'text/plain'},
});
```

Set `GCS_MIMIC_BINARY` to the integrated executable or install it on PATH. No
shell is used; arguments retain literal names/headers/query values, execution
has a ten-second timeout and output is capped. The signer binds the final
origin as required by Google's [canonical request contract](https://docs.cloud.google.com/storage/docs/authentication/canonical-requests).
It does not mutate or re-sign an SDK-produced URL or grant object permissions.

Two existing schema-safe SDK recipes remain deliberate rather than changing
the server's unresolved provider-input contract:

```js
// Replace Node lifecycle rules without the SDK's undeclared append=false query.
import {replaceLifecycleRules} from './sdk-tests/node/migration-adapters.mjs';
await replaceLifecycleRules(bucket, [{action: {type: 'Delete'}, condition: {age: 30}}]);
```

```php
// PHP Lifecycle::clearRules() emits lifecycle: []; send the documented object form.
require 'sdk-tests/php/migration_adapters.php';
\GcsMimic\Migration\clearLifecycleRules($bucket);
```

The existing Node/PHP suites exercise these adapters, and PHP's self-check
captures the official SDK request with a mock transport to verify the exact
object form without a server. Google's
[lifecycle configuration](https://docs.cloud.google.com/storage/docs/lifecycle-configurations)
uses an object with `rule`; SPEC-JSON-UNKNOWN-QUERY and SPEC-LIFECYCLE-ARRAY stay
open. These adapters do not broaden unknown-query or wrong-type acceptance.

## Exact dependency snapshot

Checked 2026-10-30 against publisher-controlled registries. The following are
the latest numeric stable/non-yanked releases observed, not remembered defaults.
All dependencies below are harness-only; the Rust application manifest and
lockfile are unchanged by this slice.

| Input | Exact version | License | Download bytes | Primary registry/source |
| --- | --- | --- | --- | --- |
| Google Java Storage JAR | 2.76.0 | Apache-2.0 | 1,536,327 | [Maven metadata](https://repo.maven.apache.org/maven2/com/google/cloud/google-cloud-storage/maven-metadata.xml) |
| Java Testcontainers JAR | 2.0.5 | MIT | 17,789,146 | [Maven metadata](https://repo.maven.apache.org/maven2/org/testcontainers/testcontainers/maven-metadata.xml) |
| .NET Storage nupkg | 5.0.0 | Apache-2.0 | 291,825 | [NuGet versions](https://api.nuget.org/v3-flatcontainer/google.cloud.storage.v1/index.json) |
| .NET Testcontainers nupkg | 4.16.0 | MIT | 1,441,518 | [NuGet versions](https://api.nuget.org/v3-flatcontainer/testcontainers/index.json) |
| Node Testcontainers tarball | 12.2.1 | MIT | 55,241 | [npm release](https://registry.npmjs.org/testcontainers/12.2.1) |
| Arrow object_store crate | 0.14.2 | MIT OR Apache-2.0 | 310,684 | [Cargo index](https://index.crates.io/ob/je/object_store) |
| Official Rust Storage crate | 1.21.0 | Apache-2.0 | 523,750 | [Cargo index](https://index.crates.io/go/og/google-cloud-storage) |
| Official Rust auth crate | 1.18.0 | Apache-2.0 | 185,594 | [Cargo index](https://index.crates.io/go/og/google-cloud-auth) |

These are package sizes, not application binary/RSS measurements. Java's graph
contains 101 dependency entries, .NET's net10.0 lock contains 24 packages,
Node's lock contains 220 dependency packages, and the standalone Rust lock
contains 291 dependency packages plus the harness. Official client graphs are intentionally separate from
the lightweight Rust runtime. The official Rust graph brings AWS-LC through
its default TLS provider; no corresponding dependency is added to the emulator.

Maintenance checks found active, nonarchived publisher repositories for
Google Cloud .NET/Rust, all three Testcontainers implementations and Apache
Arrow object_store. The old `googleapis/java-storage` repository is archived
because its [official README](https://github.com/googleapis/java-storage)
moved development to the active [Google Cloud Java monorepo](https://github.com/googleapis/google-cloud-java/tree/main/java-storage).
Recent publisher pushes were observed on 2026-10-26 through 2026-10-30.

Direct package SHA-256 values:

```text
google-cloud-storage-2.76.0.jar a61e1e3becf88e52329420a7b14251c4a6edb7e4599a197758a6a3871ece4a4a
testcontainers-2.0.5.jar 0466f481343d5f350a91274cd7bf984308cbaf90d706247fd1cf4b1a8010c2e1
Google.Cloud.Storage.V1.5.0.0.nupkg 0c35f61fec6a18b525ac6fba30dd577d6bfa373fb17acd1e52eb6bbc591e5a6e
Testcontainers.4.16.0.nupkg cab8cdb325a8229646cafa32f230deb4ef3813126e2683a103aafd7a96a1ce4a
object_store-0.14.2.crate f1796bc93603f78c5760a69f2d58badc9618d22adade0a95385bb2adbae4eb94
google-cloud-storage-1.21.0.crate 8343454507711c984d4b367ccd79fe66284b63f7e90299c800e2d407e9bbe626
google-cloud-auth-1.18.0.crate d563c7b987f40249c274a4e6a671661df6be2d084f08476dda2f0f931576cfcf
```

Node Testcontainers' npm integrity is
`sha512-JfbCyOvLdzatwgqUI3AvYCwRW0P5mCkKQVBPO8U5qSH1efC2kHvnfWmi9VHloy1rAFG2MEqWZObRH4bq0gU1kQ==`,
publisher revision `a3465510f5cc460a0fac1e696ad83781452abdfd`.
The NuGet packages record publisher revisions
`7db3d26977d60289be1a57fbaf3e7c3e26424e24` (Storage) and
`df0f931ea92111ddd96f96fce22ff85b69a58f64` (Testcontainers).

Supporting pins are Go 1.27.3 with the existing Storage/API graph, .NET SDK
10.0.403, Rust 1.99.1, bytes 1.12.1, Tokio 1.54.1, futures-util 0.3.34 and URL
2.5.8. Maven 3.10.0 is the newest stable release observed; its compiler,
exec and dependency plugins are pinned to 3.16.0, 3.6.4 and 3.11.0. Java
verification used [Temurin](https://api.adoptium.net/v3/assets/latest/27/hotspot?architecture=aarch64&image_type=jdk&os=mac&vendor=eclipse)
27.0.1.0.2+1; the source target remains Java 17. Node SDK 8.4.0 reuses the
existing current pin. Lockfiles are separate per ecosystem; Maven's deterministic
graph file is compared before execution because Maven has no native lockfile.

## Verification and remaining integration

Executed preparation on 2026-10-30:

- `go build ./...`, `go test -race ./...`, `go vet ./...`: pass for the facade and proxy; socket tests require loopback bind permission.
- Java Maven compile and `--self-check`: pass; .NET build with warnings as errors and `--self-check`: pass.
- Rust `cargo check --locked`, `cargo clippy --all-targets -- -D warnings` and one endpoint unit test: pass.
- Node adapter syntax and four unit tests: pass; Testcontainers Node syntax/origin self-check: pass.
- PHP adapter/suite syntax and mocked official-SDK lifecycle/origin self-checks: pass in the existing digest-pinned Composer image with networking disabled.
- `cargo xtask lint`: zero production violations; shell syntax and `git diff --check`: pass.

No emulator-target SDK request, Testcontainers startup, browser, real-cloud call
or performance measurement is claimed by these preparation checks. Run each runtime scenario
and the Go integration-tag tests against final source, then run Node/Java/.NET
container modes against the same final image. Exercise repeated clean teardown
and parallel invocations during that gate. Keep unsupported profiles and broader
API rows incomplete until their own acceptance passes.

The integrated native run passes the real Rust-subprocess Go facade (27 tests
including subtests, race enabled), real-source exporter, Java 2.76.0, .NET 5.0.0,
official Rust 1.21.0 and Arrow 0.14.2 scenarios. Java's absent generation becomes
an explicit body null in the pinned SDK; SPEC-015 records the narrow correction.
The .NET SDK keeps its ordinary compressed initiation and ETag condition.
Arrow uses explicit anonymous credentials and once-decoded path-style bucket
components while signatures retain the original raw path. No SDK condition,
checksum or authentication guard is removed. [Combined evidence](../evidence/migration-integration.md)
records the separate final-source and container/browser gates.

Central wiring now covers `go/go.mod`, `go/sourceexport/go.mod`, Java Maven
dependencies/plugins and runtime pins, .NET NuGet/global.json, the standalone
Rust Cargo graph and Node Testcontainers in the same daily detector and
canonical snapshot. `scripts/sdk-go.sh --with-clients` adds the native clients,
Go integration-tag tests and the real-source exporter; it exports
`GCS_MIMIC_BINARY` for the Node signer and facade. The corresponding CI lane
installs pinned tools, and `just check-client-containers IMAGE` reuses one final
image for Java/.NET/Node adapters. Actionlint and local setup checks do not
establish remote CI execution. Actual SDK/container success is recorded
separately in the combined evidence; untested profiles remain incomplete.
