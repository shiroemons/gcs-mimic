# Rust SDK scorecard probe

<!-- markdownlint-configure-file {"MD013": false} -->

Probes the official `google-cloud-storage` 1.22.0 crate against an emulator.
It emits one JSONL line per case: the 51 shared bucket, object and signed
cases, in the same IDs and order as the Python probe. It has no SDK-specific
(`sdk` area) cases. `SCORECARD_GRPC_ENDPOINT` is required because the SDK's
`StorageControl` client (used for bucket operations) speaks gRPC.

## Build and run

The probe is a standalone crate (its own workspace and lockfile). Run it in a
Linux container: on macOS the SDK verifies certificates through the system
keychain and ignores `SSL_CERT_FILE`, so it cannot trust the scorecard's
development CA there. `sdk-scorecard/run.sh` and `sdk-scorecard/test-tls.sh`
build and run it in the digest-pinned official `rust` image and forward the
container's loopback ports to the host.

```sh
cargo build --locked --release --manifest-path sdk-scorecard/rust/Cargo.toml
cargo clippy --locked --all-targets --manifest-path sdk-scorecard/rust/Cargo.toml -- -D warnings
```

On Linux the binary takes `SCORECARD_ENDPOINT`, `SCORECARD_GRPC_ENDPOINT`,
`SCORECARD_CA_CERT` and `SCORECARD_SIGNING_KEY`; `--transport-check` only
verifies CA, hostname and origin handling. The probe sets `SSL_CERT_FILE` from
`SCORECARD_CA_CERT` itself and clears proxy, credential and metadata variables.

## Host header without a port

For a non-`googleapis.com` endpoint, the SDK's HTTP requests (object uploads,
downloads, listing and the other JSON object calls) send `Host: <endpoint host>`
with the port removed (`google-cloud-gax-internal` 0.7.21, `host.rs`: "Emulators
... fall back to the passed-in service host"; `http_builder_with_url` sets the
header from it). No public builder option changes this, and the scorecard origin
needs an explicit port, so the TLS frontend accepts `Host: 127.0.0.1` as well as
`127.0.0.1:<port>`. Any other name or port is still rejected with 400, and the
frontend rewrites the outgoing Host to the full origin authority, so the backends
see the same Host as for every other SDK. Before this, 32 of 47 scored Rust cases
failed with `unexpected frontend Host` for every product.

## Unsupported cases

Cases the SDK has no public API for are emitted with `unsupported: true` and
excluded from the Rust denominator.

| Case | Reason |
| --- | --- |
| `bucket.notifications` | 1.22.0 has no public API for bucket notification configurations. |
| `project.hmac_keys` | 1.22.0 has no public API for HMAC keys. |
| `project.service_account` | 1.22.0 has no public API for the project service account. |
| `signed.v2_get` | 1.22.0 has no public API for V2 signed URLs. |

## ACL reads need a read mask

`StorageControl::get_object` without a read mask returns no `acl` or `owner`: the
pinned `google.storage.v2` proto documents the default `GetObject` mask as every
field except `metadata.acl` and `metadata.owner`. `object.acl` therefore sets
`read_mask` to `acl` for both reads. Before this, the case read the ACL without a
mask and failed with `ACL missing` against a correct emulator.
