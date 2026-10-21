# Object media verification

Verified on 2026-10-21. Runtime and regression-test source: `945d303`; documentation and
ledger checks apply to the accompanying documentation. Implementation status belongs only
to [the ledger](../../../compatibility/features.json). The
[slice contract](../slices/json-object-media.md) defines the supported subset and local rules.

## Environment and provenance

Rust 1.99.1, edition 2024, locked Cargo dependencies and the rustup proxy were used.
The host is macOS arm64; container checks run native linux/arm64 and emulated linux/amd64.
Go uses 1.27.3, Storage v1.69.0 and API v0.306.0 with the committed module graph.
Rust/SDK tests use offline dependencies and loopback; no test contacts Google Cloud or
the metadata server. Dependency checking contacts official release registries.
The [source record](object-media-sources.txt) pins Discovery revision `20261013` and
googleapis proto content digests; [the dependency snapshot](../../../compatibility/dependency-status.json)
records compiler, crate, SDK and Docker pins.

Raw logs replace machine-local path prefixes with `<checkout>` or `<local-path>`.
Current tool output retains its original whitespace, assertions and results. The guide transcript
records an executed first-use workflow; its response examples include a generation-bearing
selfLink and are not evidence for the current generation-free selfLink. Current link behavior
is asserted by the runtime wire tests. The complete guide was not rerun against `945d303`.

## Verified behavior

- JSON media upload, metadata get, whole-object download and delete preserve exact bytes,
  MIME type, size, MD5/CRC32C and the selected generation. Names are decoded once and stored
  through opaque blob IDs. Configured links ignore request Host and forwarded headers.
- Generation/metageneration match checks, empty GET not-match 304, mutation 412, live
  generation selection and nonempty-bucket 409 are covered. Eight simultaneous
  create-if-absent requests have one winner; failed requests preserve the previous object.
- After header validation and the known Content-Length cap check, an upload immediately
  acquires one of eight permits or returns 503 without a catalog round trip. One advisory
  bucket/condition check (404/412) precedes body streaming, streamed-size 413 and body/hash
  400. Publication checks the state again inside its transaction; concurrent mutations
  after the advisory check can change the final 404/412 outcome.
- Body inactivity is suspended during the advisory check, then restarted before streaming.
  A completed upload queued behind a commit blocked by an external IMMEDIATE lock beyond
  the 200 ms inactivity timeout receives 200 and retains exact bytes. Inactivity also ends
  at confirmed EOF while storage finishes. Channel closure without an explicit successful
  EOF marker aborts, including disconnected overwrites that must preserve old bytes.
- Absolute upload/media request ceilings default to 900000 ms; advisory and post-EOF
  response waits each default to 30000 ms, capped by the remaining request budget.
  CLI/environment/JSON/default precedence, inclusive upper/lower bounds and rejection
  of zero, negative, excessive and noninteger values pass for both settings.
  Eight admitted trickle uploads sending every 70 ms are closed at the 900 ms absolute
  ceiling without an HTTP error response despite a 300 ms inactivity timeout; old metadata/bytes survive, cancelled
  staging is removed, no new blob/object is published and a subsequent upload succeeds.
  A progressing 32 MiB download is truncated at a 1200 ms ceiling before 3000 ms inactivity.
  An external IMMEDIATE lock keeps a post-EOF commit and a queued advisory check waiting;
  both return 503 `backendError` at their 600 ms wait bound before the lock is released.
  Releasing the lock lets the complete body commit; the timed-out advisory request
  publishes nothing. The post-EOF mutation outcome is unknown to the client.
- A bounded close uses a write half-close and discards at most 256 KiB for at most 100 ms.
  Early 413/501/400 responses are readable on macOS loopback during an attempted 8 MiB
  body; this does not establish that lingering causes readability on every platform.
- Four-worker EOF-barrier storage races use deletion-completion and visible-generation
  rendezvous plus simultaneous starts. Both overwrite/delete and upload/bucket-delete
  outcomes are asserted without staging/blob leaks. Five standalone runs each report
  overwrite=4, object_delete=8, upload=4 and bucket_delete=8; the same race also passes
  in both complete workspace runs.
- Publication syncs staging, publishes/syncs an opaque blob, then commits the SQL reference
  and generation allocator. Actual SIGKILL cases cover incomplete staging, a renamed blob
  waiting behind an external IMMEDIATE lock before catalog commit, and restart after
  successful commit. Precommit restart retains old bytes and reclaims the unreferenced
  blob. Fixtures cover deletion journals, orphan cleanup, 128-row recovery pages and
  missing/truncated/non-file referenced data that blocks readiness.
- Committed overwrite/delete returns success when old-blob reclamation is deferred to
  restart. Repeated startup reclamation failure names the blob ID once and blocks readiness
  until repaired. The permission-dependent test first probes unlink denial and skips that
  case explicitly when elevated privileges permit unlink; a linux/arm64 root-container
  [execution](object-media-root.txt) prints `SKIP: read-only shard permits unlink (for example when running as root)`
  and exits 0. The independent startup obstruction case remains runnable as root.
- Schema v3 persists the generation high-water mark across deletion/restart/backward clocks.
  Object-free v1 migrates directly to v3 transactionally; unreleased v2 is rejected.
  Under the directory lock, WAL-free rejection uses an escaped immutable read-only URI,
  preserves database bytes and creates no WAL/SHM sidecars, including URI-reserved/Unicode
  paths. Hot-WAL rejection uses ordinary read-only mode and preserves database/WAL bytes.
  OS metadata `.DS_Store`/`._*` is ignored; other invalid entries have startup diagnostics.
- Go JSON Attrs/NewReader/Delete, both not-match read conditions, endpoint isolation,
  bucket lifecycle/paging and unsupported Writer rejection pass. Raw HTTP performs media
  upload. GET/DELETE retry tokens are accepted without deduplication; repeated DELETE
  returns 404 after the first 204. Upload tokens and unsupported semantics return 501.

## Commands and results

| Command | Result | Evidence |
| --- | --- | --- |
| `cargo +1.99.1 test --workspace --locked --offline` | PASS 2/2; 138 tests each, 0 failed/ignored | [Tests](object-media-tests.txt) |
| Object HTTP cases in the complete workspace runs | PASS 2/2; 20 tests each, including transfer/catalog ceilings and listed regressions | [HTTP output](object-media-http-repeat.txt) |
| Advisory precedence and explicit no-EOF storage cases in the complete workspace runs | PASS 2/2; immediate/300 ms delayed body and producer-closure cases | [Tests](object-media-tests.txt) |
| Four-worker EOF-barrier storage races in the complete workspace runs | PASS 2/2; both mutation outcomes asserted | [Tests](object-media-tests.txt) |
| `cargo +1.99.1 fmt --all --check` | PASS, exit 0 | [Fmt](object-media-fmt.txt) |
| `cargo +1.99.1 clippy --workspace --all-targets --locked --offline -- -D warnings` | PASS, exit 0 | [Clippy](object-media-clippy.txt) |
| `cargo +1.99.1 xtask compatibility validate`, `render`, `check` | PASS; 588 rows, 401 parameter locations and 357 schema fields; both views match | [Compatibility](object-media-compatibility.txt) |
| `cargo +1.99.1 xtask dependencies check` | PASS, exit 0; every checked input CURRENT | [Dependencies](object-media-dependencies.txt) |
| `GOPROXY=off GOFLAGS=-mod=readonly bash scripts/sdk-go.sh` | PASS, once on final runtime source | [Go SDK](object-media-go.txt) |
| `bash scripts/smoke-containers.sh` | PASS, once on final runtime source; native arm64/emulated amd64 | [Containers](object-media-containers.txt) |
| Executed getting-started `sh` blocks in one `bash -euxo pipefail` session | PASS; bounded by the guide provenance above | [Guide](object-media-guide.txt) |
| Markdownlint on changed Markdown | PASS, 0 issues | [Markdownlint](object-media-markdownlint.txt) |
| Scoped internal-label and local-path scans; relative Markdown links | No matches; all checked paths resolve | [Documentation scans](object-media-local-path-scan.txt) |

Each complete workspace run contains 49 library, 6 bucket HTTP, 12 catalog, 10 foundation,
20 object HTTP, 21 object storage, 7 xtask library, 11 compatibility and 2 dependency tests.
The guide covers host/peer media, checksums/bytes, 412/304/409, stop/start and down/up
identity persistence, deletion/404, explicit volume reset, Compose Go SDK, native release
build/probes/upload/download and SIGTERM. Its disposable containers/networks/volumes/tags
were removed. The final container smoke also removed only its own resources and exact
image tags; [cleanup output](object-media-container-cleanup.txt) records no scoped residue.
No shared-resource pruning was performed.

Documentation checks disable only MD013 for existing long API tables, URLs and commands;
MD024 uses `siblings_only: true` because generated matrices repeat category headings under
different kind parents. These changes do not modify runtime/test source, statuses or fidelity.

## Catalog-timeout regression verification

Checked on 2026-10-22 with Rust 1.99.1, locked offline dependencies and macOS arm64.
The unchanged test reproduced the failure with `RUST_BACKTRACE=1`: both upload 503
assertions passed, then the media GET after `ROLLBACK` reached EOF before response headers.
The diagnostic and relevant backtrace frames were:

```text
connection deadline or object body inactivity timeout exceeded
assertion `left != right` failed: HTTP response headers
  left: 0
 right: 0
5: <object_http::Server>::raw
6: <object_http::Server>::request
7: <object_http::Server>::get
8: object_http::obj_http_catalog_wait_deadlines_return_backend_error_and_commit_can_finish
   at ./tests/object_http.rs:765:23
```

The failure was a test synchronization error. Releasing the external SQLite IMMEDIATE
lock does not wake a sleeping busy-handler retry immediately. The bundled SQLite
`sqliteDefaultBusyCallback` retries with sleeps reaching 100 ms. The follow-up GET could
therefore exceed its unchanged 100 ms response-startup deadline while the background
commit still held catalog execution. Upload advisory/post-EOF waits correctly suspend
inactivity and return 503; GET startup does not use that suspension. The test now observes
the committed row through the external SQLite connection, bounded to two seconds, before
checking the media bytes. It retains both 503 `backendError` assertions, the original
500 ms–2 s bound before lock release, queued-upload 404 and subsequent-upload success.
No server behavior, configuration, feature status or fidelity changed.

The [official JSON status reference](https://docs.cloud.google.com/storage/docs/json_api/v1/status-codes)
was rechecked on 2026-10-22 for 503 `backendError`; the timeout budgets and suspension
rules are emulator-local contracts in the [slice](../slices/json-object-media.md).

| Command | Result |
| --- | --- |
| `for i in $(seq 30); do cargo test --locked --offline --test object_http obj_http_catalog_wait_deadlines \|\| exit; done` | PASS, 30 consecutive standalone runs; no retries/skips |
| `for i in $(seq 5); do cargo test --locked --offline --test object_http \|\| exit; done` | PASS, 5 consecutive runs, 20 tests each |
| `cargo test --workspace --locked --offline` | PASS, 154 tests; 0 failed/ignored |
| `cargo fmt --all --check` | PASS |
| `cargo clippy --workspace --all-targets --locked --offline -- -D warnings` | PASS |
| `cargo xtask compatibility validate` | PASS; 591 feature rows, 401 parameter locations, 357 schema fields |
| `GOPROXY=off GOFLAGS=-mod=readonly GCS_MIMIC_GO_TEST_RUN=TestObjectMediaLifecycle bash scripts/sdk-go.sh` | PASS, once; Go vet and the existing object lifecycle test |

Markdownlint v0.24.0 checks both changed documents with only MD013 disabled for existing
long tables, URLs and commands.

Container builds, the full SDK suite and real-cloud checks were not rerun for this test-only
change; the earlier evidence above retains its original scope.

## Remaining limits and next slice

Whole object method rows remain PARTIAL. Multipart/resumable uploads, ranges, listing,
metadata mutation, historical versions, auth/IAM/ACL, encryption, signed URLs and Ruby/Rails
remain outside this slice. SPEC-008..013 retain local condition precedence, direct-200
media delivery, hash/MIME/header/opaque-resource choices and retry-token limitations.
DESIGN-075 remains IMPLEMENTED only for its existing opaque-path project scope.

Complete bodies may commit after disconnect, shutdown or post-EOF response timeout.
After a post-EOF timeout the mutation outcome is unknown: re-read state or retry with
generation/metageneration preconditions. Upload advisory and post-EOF response waits are
bounded, but background filesystem/SQLite work and shutdown catalog close have no separate
execution bound. Work retains directory ownership and permits until it finishes;
permanently stalled filesystem work can therefore still retain permits. SQLite lock waits
retain the five-second busy timeout. Absolute deadlines prevent clients from retaining
upload slots indefinitely solely by trickling body chunks. Immutable preflight does not consider a
hot rollback `-journal`, which gcs-mimic never creates in WAL mode but a foreign DELETE-mode
catalog could. Generation nonreuse is established for identities allocated by v3 from an
empty object catalog; accepted intermediate v3 catalogs do not reconstruct deleted history.

Every instruction-level fsync/deletion crash window, power loss, a permanently stalled
filesystem, native amd64, Windows, remote CI, real-cloud differential behavior, a 1 GiB
constrained-container transfer and RSS/cgroup/CPU/latency measurements remain unverified.
The next slice is object listing and metadata; JSON multipart follows separately.
