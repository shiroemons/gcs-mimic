# Two independent Rust client contracts

This standalone package checks Google's official `google-cloud-storage` 1.21.0
and Apache Arrow's `object_store` 0.14.2 separately. Neither is added to the
emulator's workspace or production dependency graph. `Cargo.lock` is committed.

```sh
cargo check --locked --manifest-path sdk-tests/rust/Cargo.toml
cargo test --locked --manifest-path sdk-tests/rust/Cargo.toml --lib
GCS_MIMIC_ENDPOINT=http://127.0.0.1:4443 \
  GCS_MIMIC_GRPC_ENDPOINT=127.0.0.1:4444 bash sdk-tests/rust/run.sh
```

Start the emulator with distinct HTTP and gRPC listeners and pass their exact
addresses above. The gRPC variable is a bare `host:port`, with no scheme; both
addresses must be explicit loopback destinations. There is no cloud default.

The runner starts each **compiled** executable under `go/cmd/sdk-guard` with
its opt-in `--grpc-endpoint` mode. Go's standard HTTP server provides a test-only
HTTP/1 and prior-knowledge h2c frontdoor. Each client receives that frontdoor as
its single SDK endpoint: ordinary HTTP goes to the fixed HTTP backend, while
`application/grpc` calls in the Storage v1/v2 RPC namespaces go to the distinct
fixed gRPC backend. This adapter does not add a combined production listener.

The guard replaces proxy variables, clears ambient credential discovery settings
and rejects foreign hosts/ports, metadata requests, userinfo and CONNECT.
Absolute HTTP proxy requests can target only the frontdoor or the selected HTTP
backend, retaining guarded resumable session URLs. Relative requests require the
exact frontdoor Host. Upstream transports never chain through ambient proxies.
HTTP bodies and gRPC frames/trailers stream through bounded buffers, with eight
active requests, sixteen client connections and a two-minute child budget.
The wrapper fails on a blocked request or when no HTTP/gRPC request traverses it.
Direct executable invocation without its environment fails. This is a test
harness boundary, not an OS egress sandbox; code that deliberately opens an
unrelated socket is outside its isolation guarantee.

The official SDK uses explicit anonymous credentials. Arrow uses its documented
`disable_oauth` service-account option with empty key/email fields as well as
`skip_signature`, because this pinned release still fetches PUT credentials
despite `skip_signature`. No real key, ADC file or metadata token is used.
Arrow also receives the proxy explicitly. Both retain their published SDK code
and locked dependencies.

The official binary creates a unique bucket, forces a resumable upload and
checks generation/metadata and bounded streamed reads. The Arrow binary uses
XML PUT/GET/range/list and copy-if-absent, then verifies a duplicate-copy failure.
Both clean known fixtures through the official control client. Fixed fixture
payloads are small; no unbounded object catalog or arbitrary body is collected.

The official Rust scenario has passed against the isolated permissive HTTP/gRPC
server through this frontdoor, with five HTTP and four gRPC requests. Arrow's
XML scenario and cleanup also passed, with six HTTP and five gRPC requests.
Neither run had a blocked request. Go guard race tests, Rust check, clippy,
formatting and the endpoint parser unit test pass. The feature ledger owns
status; this does not
claim complete API parity or treat Arrow as official Google SDK evidence.
