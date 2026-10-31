# Rust SDK scorecard probe

<!-- markdownlint-configure-file {"MD013": false} -->

Probes the official `google-cloud-storage` 1.21.0 crate against an emulator.
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

## Unsupported cases

Cases the SDK has no public API for are emitted with `unsupported: true` and
excluded from the Rust denominator.

| Case | Reason |
| --- | --- |
| `bucket.notifications` | 1.21.0 has no public API for bucket notification configurations. |
| `project.hmac_keys` | 1.21.0 has no public API for HMAC keys. |
| `project.service_account` | 1.21.0 has no public API for the project service account. |
| `signed.v2_get` | 1.21.0 has no public API for V2 signed URLs. |
