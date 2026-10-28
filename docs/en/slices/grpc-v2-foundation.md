# Storage v2 gRPC foundation

<!-- markdownlint-configure-file {"MD013": false} -->

This document records the foundation-only delivery and its original rejection
tests. Current Storage v2 operation subsets are described in the
[bucket](grpc-v2-buckets.md), [object I/O](grpc-v2-object-io.md),
[update/copy](grpc-v2-update-copy.md) and [bidi read](grpc-v2-bidi-read.md)
contracts; CancelResumableWrite is now part of the object I/O contract. Rapid
append and unsupported fields remain UNIMPLEMENTED; the legacy Storage v1 migration profile is described in [its contract](grpc-v1-legacy.md).
The all-RPC rejection statements below describe the original foundation snapshot.

Scope: BEHAVIOR-RPC-005 and the unsupported gRPC portion of DESIGN-082. RPC-STORAGE-001–024 retain their existing status: this slice supplies an entry point, not their implementation. At delivery, Storage v1, Rapid append, CancelResumableWrite and every operation's business logic remained unsupported. The strict-authentication and wire-status Go checks that originally used CancelResumableWrite as the unsupported probe now follow its object I/O behavior; see [object I/O](grpc-v2-object-io.md).

## Acceptance contract

| Case | Required behavior |
| --- | --- |
| grpc_listener_disabled_by_default | Without grpc_listen, no gRPC socket is opened and HTTP/HTTPS startup is unchanged. |
| grpc_listener_starts_when_configured | JSON grpc_listen and GCS_MIMIC_GRPC_LISTEN enable a separate plaintext h2c socket; malformed values and occupied ports stop startup with a cause and an example remedy. |
| grpc_listener_follows_startup_and_shutdown_order | Bind follows recovery, initialization and seed; serving follows ready. SIGTERM stops admission and drains gRPC alongside HTTP before catalog close. |
| grpc_unimplemented_is_immediate_and_actionable | Every pinned Storage v2 RPC, unknown service and unknown method fails immediately with UNIMPLEMENTED, never UNAVAILABLE; google.rpc.Status contains ErrorInfo identifying gcs-mimic's unsupported operation and the JSON API remedy. |
| grpc_go_client_fails_fast | Ordinary storage.NewGRPCClient(ctx) uses STORAGE_EMULATOR_HOST_GRPC; Bucket.Attrs, Writer through Close and NewMultiRangeDownloader fail well before their deadline, are not retryable, and differ from not-found. |
| grpc_resource_name_parsing | Parse projects/_/buckets/NAME and object names without losing slashes or encoded bytes; malformed resource names return INVALID_ARGUMENT. |
| grpc_record_conversion | Convert shared BucketRecord/ObjectRecord fields to their protobuf counterparts with the same values as JSON. Unsupported protobuf fields remain empty. |
| grpc_auth_metadata_handoff | Permissive ignores credentials; strict resolves authorization Bearer through shared Auth and passes the local principal or anonymous Caller to the RPC group. Invalid tokens fail in the entry point; the groups' common unsupported-response function rejects anonymous callers with UNAUTHENTICATED until operation-specific shared authorization is implemented. |
| grpc_idle_connection_closes_with_goaway | With no open streams, an inactive connection sends graceful GOAWAY before closing and releases shared HTTP admission. |
| grpc_idle_never_cuts_in_flight_streams | A request remains active until its response body ends or is dropped; connection_timeout_ms idle expiry does not cut open streams, but the separate silence bound applies. A Drop guard releases the count on cancellation. |
| grpc_dead_peer_is_detected_by_keepalive | HTTP/2 keepalive detects a silent peer with an open stream and releases its connection slot; interval and acknowledgement timeout derive from connection_timeout_ms. |
| grpc_goaway_has_bounded_grace | Idle and SIGTERM GOAWAY have a bounded grace period, including incomplete handshakes and streams that never finish, without changing shutdown order. |
| grpc_silent_open_stream_is_bounded | An open stream with no request/response HEADERS, DATA or trailers for 60 seconds triggers GOAWAY, then closes within the existing bounded grace and releases shared HTTP admission; PING/PONG does not count as activity. |
| grpc_active_stream_survives_past_bound | Request or response frames arriving at intervals shorter than the silence bound keep a stream alive beyond that bound in total elapsed time. |

The fixed internal 60-second silence bound permits legitimate waits for another Go MultiRangeDownloader range while bounding abnormal inactivity in local development; no setting is added.

The new setting is needed because Go's gRPC emulator transport requires its own plaintext endpoint; HTTP/HTTPS cannot provide that endpoint. Default: absent/disabled. CLI `--grpc-listen 127.0.0.1:4444`, environment `GCS_MIMIC_GRPC_LISTEN=127.0.0.1:4444`, or flat JSON `{"grpc_listen":"127.0.0.1:4444"}` follows CLI > environment > file > default precedence. It never changes the advertised HTTP/public URL.

## Boundaries

Generated code is committed; ordinary source and Docker builds do not require protoc. The pinned upstream protobuf closure and generated-code provenance are maintained under src/grpc. Subsequent buckets, object_io, update_copy and bidi_read slices own their separate modules and tests. The shared service exposes state, authentication, status, resource parsing and record conversion without changing storage.

No object catalog scan, body collection, polling timer or benchmark is added. Message, connection and stream admission are bounded. Business operations and provider-exact success/error semantics remain pending; UNIMPLEMENTED is an explicit emulator limitation.

The Hyper HTTP/2 driver uses the existing shared `max_connections` limit (default 32), eight concurrent streams per connection, 16 KiB header lists, 64 KiB send buffers and 4 MiB encoded/decoded protobuf messages. HTTP/HTTPS and gRPC stop admission, drain within the existing shutdown deadline and then close the catalog together. No independent transport runtime, router, client, compression or TLS stack is added. Another generated service, including future v1, can be routed on this h2c driver.

`connection_timeout_ms` bounds gRPC inactivity when no requests are active, including an incomplete HTTP/2 handshake. A watch counter increments at request admission and a Drop guard decrements it when the response body ends, errors or is dropped, or the response future is cancelled. Pending responses and quiet streaming bodies are not cut by that idle deadline, but a separate fixed 60-second silence bound applies while streams are open. One connection-wide last-frame timestamp is updated by request/response headers and incoming/outgoing DATA or trailers; HTTP/2 PING/PONG never updates it. HTTP/2 keepalive uses `connection_timeout_ms` for both its ping interval and acknowledgement timeout. A dead peer with an open stream is closed on missing ping acknowledgement. One application timer serves idle expiry, stream silence and GOAWAY grace, and watch notifications wake it on stream or frame changes; no per-stream timer or polling loop is added. Tests inject a shorter silence bound into the same connection function; test services remain under `cfg(test)`.

Idle GOAWAY grace uses `connection_timeout_ms`; SIGTERM caps it at `min(connection_timeout_ms, shutdown_timeout_ms / 2)` to reserve half the outer drain budget for connection cleanup and joining, including when idle GOAWAY is already underway. This keeps incomplete handshakes and unfinished streams bounded without changing shutdown order, and avoids racing the shared outer deadline when both settings have their default value of 5000 ms. Idle expiry sends graceful GOAWAY before closing. A peer that never completes the HTTP/2 preface cannot receive GOAWAY, and is forcibly closed at the grace deadline. Idle closure logs at Debug; keepalive failure and forced grace expiry log their cause and an example configuration remedy. For SIGTERM grace, both timeout settings affect the remedy. The unsupported remedy uses `api_base_url` from the server ready event instead of assuming a scheme, host or port.

Stream-silence expiry uses the same `connection_timeout_ms` GOAWAY grace and SIGTERM cap. It logs at Info with the cause (an open stream without send/receive frames for 60 seconds) and remedy (send periodically or close and reopen the stream).

The existing [Hyper 1.12.0 HTTP/2 builder](https://docs.rs/hyper/1.12.0/hyper/server/conn/http2/struct.Builder.html#method.keep_alive_interval) supplies keepalive interval/acknowledgement timeout and the Tokio timer; no dependency or configuration key is added. The existing [BodyExt frame adapter](https://docs.rs/http-body-util/0.1.5/http_body_util/trait.BodyExt.html#method.map_frame) tracks DATA and trailers without collecting bodies.

Bucket conversion covers resource name/ID, original owning project identifier, location/type, class, labels, create/update times, metageneration, CORS, versioning, default event hold, retention, soft deletion and IAM configuration. Object conversion covers bucket/name, generations, size/class, fixed/custom metadata, checksums (no MD5 for composite objects), component count, timestamps, holds, custom time and retention expiration. Timestamp nanoseconds are preserved. Invalid stored customTime returns INTERNAL rather than disappearing. Lifecycle, ETags, ACLs, encryption and other unsupported proto fields remain empty; dynamic public cache defaults and project-number resolution belong to subsequent operation slices. Google returns numeric project identifiers, while the local shared record retains the supplied project ID; this is an explicit local conversion limit.

The resource parser accepts exactly `projects/_/buckets/NAME`; the object name is a separate string in Storage v2, not a fabricated `/objects/` suffix. It retains slashes and literal percent encodings and validates nonempty names, object UTF-8 byte bounds and control characters. Future operations retain responsibility for semantic bucket validation.

## Sources and generation

Verified 2026-10-27: [Storage v2 proto](https://github.com/googleapis/googleapis/blob/933dd1be8180a22bca2dc473b53d4f4297ee3b0c/google/storage/v2/storage.proto), googleapis commit `933dd1be8180a22bca2dc473b53d4f4297ee3b0c`, Storage SHA-256 `a0df6132967b97a89be248f1f24d277ee8df8f8d885db1d4af6249469ff0c1cf`. Every google/api, google/iam/v1, google/rpc and google/type import is from that same commit. Googleapis does not vendor the well-known google/protobuf files; those use [protobuf v36.2](https://github.com/protocolbuffers/protobuf/tree/2c74169b34066ceb8ddb6b882fcb3fb32d737a55), commit `2c74169b34066ceb8ddb6b882fcb3fb32d737a55`, matching the regeneration compiler `libprotoc 36.2`. All 21 proto digests are in `src/grpc/proto/SHA256SUMS`; upstream licenses are alongside them. `compatibility/upstream/source-manifest.json` connects these pins to the committed generated sources.

From the repository root, with protoc 36.2 installed only for regeneration:

```sh
shasum -a 256 -c src/grpc/proto/SHA256SUMS
cargo run -p grpc-codegen --locked --offline
cargo fmt --all
git diff -- src/grpc/generated
```

The generator uses the committed import closure; it does not fetch upstream content. Ordinary `cargo build --locked` uses only the generated Rust files.

## Dependency review

Official stable Cargo index and publisher release checks on 2026-10-27 establish Rust 1.99.1/edition 2024, [tonic 0.14.7](https://github.com/grpc/grpc-rust/releases) and [prost 0.14.4](https://github.com/tokio-rs/prost/releases/tag/v0.14.4). Both publishers continue releasing maintained packages; releases are non-yanked and non-prerelease. Tonic's 0.14 family split its Prost codec/generator into the approved tonic-prost/tonic-prost-build packages; using the official split avoids a custom codec. Only the explicit workspace generation tool depends on tonic-prost-build, so application and Docker builds require no protoc.

| Direct crate | Version | License | Compressed source bytes | Minimum Rust |
| --- | --- | --- | --- | --- |
| tonic | 0.14.7 | MIT | 116713 | 1.88 |
| tonic-prost | 0.14.7 | MIT | 7916 | 1.88 |
| prost | 0.14.4 | Apache-2.0 | 35717 | 1.85 |
| prost-types | 0.14.4 | Apache-2.0 | 51454 | 1.85 |
| tonic-prost-build (regeneration only) | 0.14.7 | MIT | 15685 | 1.88 |

Sizes/licenses/minimum compilers come from the downloaded official crate archives and their manifests, checked against the registry resolution. These compatible licenses add no unapproved direct runtime crate. Hyper's existing dependency adds HTTP/2; tonic enables only codegen, using the already owned connection driver. Cargo.lock pins runtime and generator graphs. Daily freshness checks read all workspace members, including grpc-codegen. Go grpc/protobuf were already transitive dependencies; wire tests promote them to direct requirements. Detection found [grpc-go v1.85.0](https://github.com/grpc/grpc-go/releases/tag/v1.85.0), which replaces v1.84.0 with its required compatible transitive updates; protobuf remains v1.36.12. The SDK itself remains Storage v1.70.0/API v0.309.0. go.mod/go.sum record the final graph.

## Verification

Acceptance was recorded before implementation. On the unchanged listener-less baseline, the new Go test observed Bucket.Attrs and bidi multi-range exhausting five-second deadlines after UNAVAILABLE retries; Writer returned UNAVAILABLE, also failing the required assertion. Multi-range requires `storage.WithGRPCBidiReads()` in the pinned SDK; other calls use ordinary `storage.NewGRPCClient(ctx)` and the emulator environment alone.

On 2026-10-27:

- `cargo check --workspace --all-targets --locked --offline`: PASS before tests.
- `cargo test --locked --offline --lib grpc::`: PASS, resource names, record conversion (including corrupt customTime) and strict/permissive principal handoff.
- `cargo test --locked --offline --lib grpc_listener_configuration_precedence_and_diagnostics`: PASS, file/environment/CLI precedence, disabled default and invalid value remedies.
- `cargo test --locked --offline --bin gcs-mimic grpc_listener_disabled_by_default_and_bound_after_initialization`: PASS, disabled default, initialization failure before bind, configured bind and occupied-port guidance.
- `GCS_MIMIC_GO_TEST_RUN=TestGRPCFoundation bash scripts/sdk-go.sh`: PASS, Go vet, three high-level calls with five-second deadlines/two-second fail-fast assertions, all 24 RPCs, unknown service/method, canonical ErrorInfo and strict credentials. Streaming requests reject without receiving any first message or EOF. The runner SIGTERMs both HTTP/gRPC processes and waits for successful cleanup.
- `PROTOC=unavailable-protoc cargo build --locked --offline`: PASS; ordinary builds do not invoke protoc.
- `shasum -a 256 -c src/grpc/proto/SHA256SUMS`: PASS for the full pinned closure.
- `cargo test --locked --offline --test grpc_foundation`: PASS; an idle h2c connection initially owns the sole shared slot, expires to admit HTTP, then real SIGTERM drains an incomplete handshake, closes both listeners, emits stopped and releases the catalog for immediate reopen.
- `just check-sdk`: PASS on the final sources and grpc-go v1.85.0, including Go vet, the full Go HTTP suite with all gRPC foundation checks, Ruby self-check/migration/permissive/strict suites and signing. The existing Go V2 signed-transfer case skips HTTP because its SDK emits HTTPS; no new gRPC case is skipped.
- `cargo xtask dependencies check`: the first attempt hit a 30-second Go proxy timeout; the one permitted retry completed detection and returned UPDATE_AVAILABLE for grpc-go v1.85.0. All Rust runtime/generation dependencies, toolchains and other checked inputs were CURRENT. The detected gRPC update was applied before final SDK verification; the broad network detector was not repeated again.

- `RUST_TEST_THREADS=8 just check`: PASS on the final sources; formatting, native and Windows/Linux cross-target clippy, all Rust unit/integration/doc tests (no failures or ignored tests), ledger/generated views and offline script/publication checks.
- Markdownlint: PASS for the ten changed guide/contract/plan documents.

No benchmark, container build, full guide run, HTTPS-mode SDK run or real-cloud request is part of this slice. BEHAVIOR-RPC-005 and DESIGN-082 remain PARTIAL; operation rows retain their original status and fidelity. The behavior row keeps achieved fidelity NONE because emulator rejection is not provider operation conformance. Next: the four independent v2 operation groups.

### Idle connection follow-up verification

The four idle/keepalive/grace acceptance cases were recorded before implementation. Final connection sources were verified on 2026-10-27:

- `cargo check --workspace --all-targets --locked --offline`: PASS before tests.
- `cargo test --locked --offline --lib grpc::`: PASS, eight tests including GOAWAY before normal EOF, pending-response and streaming-body gaps, response Drop, missing ping acknowledgement and bounded grace.
- `cargo test --locked --offline --test grpc_foundation`: PASS, two process tests covering shared HTTP admission, both listeners closing, catalog reopen and successful SIGTERM exit with an admitted peer that sends no preface under the default 5000 ms connection/shutdown settings.
- `GCS_MIMIC_GO_TEST_RUN=TestGRPCFoundation bash scripts/sdk-go.sh`: PASS on the final connection sources, including Go vet and every foundation case.
- `RUST_TEST_THREADS=8 just check`: PASS on the final connection sources, including native/Windows/Linux clippy, all Rust unit/integration/doc tests, ledger/generated views and offline script checks.
- `cargo xtask compatibility render`: PASS; English/Japanese matrices have no content change because status, fidelity and SDK-evidence counts are unchanged.

BEHAVIOR-RPC-005 stays PARTIAL with four additional PASS cases; operation-specific behavior remains pending. The full HTTP/Ruby SDK suite was not repeated because HTTP behavior is unchanged; container, guide, benchmark and real-cloud runs were excluded.

### Open-stream silence verification

The two silence acceptance cases were recorded in this contract and the ledger before implementation. On 2026-10-27, official stable-channel/Cargo-index checks confirmed the unchanged Rust 1.99.1, Hyper 1.12.0 and Tokio 1.54.1 inputs.

- `cargo check --workspace --all-targets --locked --offline`: PASS before tests.
- `cargo test --locked --offline --lib grpc::`: PASS, ten tests. The injected short bound covers silent pending responses and open response bodies while acknowledging PING, GOAWAY before EOF and bounded connection-task completion; separate incoming/outgoing DATA streams survive more than twice the bound. The four existing connection cases and Drop counting also pass.
- `GCS_MIMIC_GO_TEST_RUN=TestGRPCFoundation bash scripts/sdk-go.sh`: PASS, Go vet and every foundation case on the final connection sources.
- `RUST_TEST_THREADS=8 just check`: PASS on the final connection sources; native/Windows/Linux clippy, all Rust unit/integration/doc tests (including both real-process gRPC tests), ledger/generated views and offline script checks. No failures or ignored tests.
- `cargo xtask compatibility render`: PASS; both matrices regenerate without content changes because status, fidelity and SDK-evidence counts are unchanged.

BEHAVIOR-RPC-005 remains PARTIAL with two additional PASS cases. Production uses the fixed 60-second bound; tests inject a short value instead of waiting 60 seconds. The bound is connection-wide, so activity on any stream renews it. Full HTTP/Ruby SDK, container, guide, benchmark and real-cloud runs are excluded from this transport-only change.
