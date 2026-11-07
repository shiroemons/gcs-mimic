# Java SDK scorecard probe

<!-- markdownlint-configure-file {"MD013": false} -->

Probes the official `com.google.cloud:google-cloud-storage` 2.78.0 against an
emulator. It emits one JSONL line per case: the 51 shared bucket, object and
signed cases (same IDs and order as the Python probe) plus 19 Java-specific
cases in the `sdk` area (IDs start with `sdk.`). The area covers the SDK's
blob handles, channels, write sessions, Transfer Manager, batch, retry and
gRPC transport.

## Build and run

Needs the JDK in `.java-version` and Maven in `.maven-version`.

```sh
bash sdk-scorecard/java/run.sh build   # verifies dependencies.lock, compiles, records the classpath
SCORECARD_ENDPOINT=https://127.0.0.1:PORT SCORECARD_GRPC_ENDPOINT=127.0.0.1:GRPC_PORT \
  SCORECARD_CA_CERT=ca.pem SCORECARD_SIGNING_KEY=fixtures/signing/test-only-private.pem \
  bash sdk-scorecard/java/run.sh run   # JSONL on stdout
```

`run.sh run --transport-check` only verifies CA, hostname and origin handling.
`SCORECARD_RUN=<substring>` restricts the run to matching case IDs.

## Unsupported cases

None. Every shared case is expressible with the public Java API.

## Dependencies

`pom.xml` pins `google-auth-library-oauth2-http` only for offline RSA signing
of signed URLs and POST policies, at the version the SDK already resolves. It
carries no comments because the daily dependency check accepts only literal
elements.
