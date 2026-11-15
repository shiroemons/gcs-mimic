# Java SDK scorecard probe

<!-- markdownlint-configure-file {"MD013": false} -->

Probes the official `com.google.cloud:google-cloud-storage` 2.78.0 against an
emulator. It emits one JSONL line per case: the 56 shared bucket, object and
signed cases (same IDs and order as the Python probe), 10 cases in the `grpc`
area (IDs start with `grpc.`; eight are shared with the other SDKs that have
a public gRPC API, two are Java-only write-session cases) and 14 cases in the
`sdk` area (IDs start with `sdk.`). Five of those are shared with the other
SDKs that reach the same workflow: `sdk.resumable_resume` (a `WriteChannel`
restored from its captured state), `sdk.json_batch` (`StorageBatch`),
`sdk.sliced_download` and `sdk.parallel_composite_upload` (Transfer Manager)
and `sdk.file_upload_download` (`createFrom` and `downloadTo`). The other nine
cover the SDK's blob handles, channels, write sessions and Transfer Manager
uploads.

Which bytes a resumed upload sends and how a download is sliced are observed
on the wire: those cases build their client with transport options that
record each request's `Content-Range`, `Range` and body length just before it
is sent (`RequestLog`).

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

`bucket.soft_delete_restore`: google-cloud-storage 2.78.0 has no public API for
restoring a soft-deleted bucket (`Storage.restore` takes only a `BlobId`). Every
other shared case is expressible with the public Java API.

## Dependencies

`pom.xml` pins `google-auth-library-oauth2-http` only for offline RSA signing
of signed URLs and POST policies, at its newest release. That can be ahead of
the version the SDK resolves; `google-auth-library-credentials` then stays at
the SDK's version. `pom.xml` carries no comments because the daily dependency
check accepts only literal elements.
