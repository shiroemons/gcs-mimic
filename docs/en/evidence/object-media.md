# Object media verification

Verified on 2026-10-21. Runtime and regression-test source: `9a9546c`; documentation and
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

Raw logs replace machine-local paths with `<local-path>` and normalize trailing whitespace
and blank lines at EOF. Tool output, assertions and results are retained. The guide transcript
records an executed first-use workflow; its response examples include a generation-bearing
selfLink and are not evidence for the current generation-free selfLink. Current link behavior
is asserted by the runtime wire tests. The complete guide was not rerun against `9a9546c`.

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
- A bounded close uses a write half-close and discards at most 256 KiB for at most 100 ms.
  Early 413/501/400 responses are readable on macOS loopback during an attempted 8 MiB
  body; this does not establish that lingering causes readability on every platform.
- Four-worker EOF-barrier storage races use deletion-completion and visible-generation
  rendezvous plus simultaneous starts. Both overwrite/delete and upload/bucket-delete
  outcomes are asserted without staging/blob leaks. Five standalone runs each report
  overwrite=4, object_delete=8, upload=4 and bucket_delete=8; the same race also passes
  in all three complete workspace runs.
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
| `cargo +1.99.1 test --workspace --locked --offline` | PASS 3/3; 134 tests each, 0 failed/ignored | [Tests](object-media-tests.txt) |
| `cargo +1.99.1 test --locked --offline --test object_http` | PASS 5/5; 17 tests each | [HTTP repeats](object-media-http-repeat.txt) |
| `cargo +1.99.1 test --locked --offline --test object_http obj_http_advisory_failures_precede_chunked_body_errors_regardless_of_timing -- --exact` | PASS 20/20; immediate and 300 ms delayed body cases | [Precedence repeats](object-media-precedence-repeat.txt) |
| `cargo +1.99.1 test --locked --offline --test object_storage conditional_overwrite_delete_and_bucket_delete_upload_races_are_atomic -- --exact --nocapture` | PASS 5/5 standalone executions | [Race repeats](object-media-races.txt) |
| `cargo +1.99.1 fmt --all --check` | PASS, exit 0 | [Fmt](object-media-fmt.txt) |
| `cargo +1.99.1 clippy --workspace --all-targets --locked --offline -- -D warnings` | PASS, exit 0 | [Clippy](object-media-clippy.txt) |
| `cargo +1.99.1 xtask compatibility validate`, `render`, `check` | PASS; 588 rows, 401 parameter locations and 357 schema fields; both views match | [Compatibility](object-media-compatibility.txt) |
| `cargo +1.99.1 xtask dependencies check` | PASS, exit 0; every checked input CURRENT | [Dependencies](object-media-dependencies.txt) |
| `GOPROXY=off GOFLAGS=-mod=readonly bash scripts/sdk-go.sh` | PASS, once on final runtime source | [Go SDK](object-media-go.txt) |
| `bash scripts/smoke-containers.sh` | PASS, once on final runtime source; native arm64/emulated amd64 | [Containers](object-media-containers.txt) |
| Executed getting-started `sh` blocks in one `bash -euxo pipefail` session | PASS; bounded by the guide provenance above | [Guide](object-media-guide.txt) |
| Markdownlint on changed Markdown | PASS, 0 issues | [Markdownlint](object-media-markdownlint.txt) |
| Scoped internal-label and local-path scans; relative Markdown links | No matches; all checked paths resolve | [Documentation scans](object-media-local-path-scan.txt) |

Each complete workspace run contains 48 library, 6 bucket HTTP, 12 catalog, 10 foundation,
17 object HTTP, 21 object storage, 7 xtask library, 11 compatibility and 2 dependency tests.
The guide covers host/peer media, checksums/bytes, 412/304/409, stop/start and down/up
identity persistence, deletion/404, explicit volume reset, Compose Go SDK, native release
build/probes/upload/download and SIGTERM. Its disposable containers/networks/volumes/tags
were removed. The final container smoke also removed only its own resources and exact
image tags; [cleanup output](object-media-container-cleanup.txt) records no scoped residue.
No shared-resource pruning was performed.

Documentation checks disable only MD013 for existing long API tables, URLs and commands;
MD024 uses `siblings_only: true` because generated matrices repeat category headings under
different kind parents. These changes do not modify runtime/test source, statuses or fidelity.

## Remaining limits and next slice

Whole object method rows remain PARTIAL. Multipart/resumable uploads, ranges, listing,
metadata mutation, historical versions, auth/IAM/ACL, encryption, signed URLs and Ruby/Rails
remain outside this slice. SPEC-008..013 retain local condition precedence, direct-200
media delivery, hash/MIME/header/opaque-resource choices and retry-token limitations.
DESIGN-075 remains IMPLEMENTED only for its existing opaque-path project scope.

Complete bodies may commit after disconnect or shutdown. Post-EOF queued writes, fsync,
rename, directory sync and shutdown catalog close have no wall-clock bound; SQLite lock
waits retain the five-second busy timeout. Advisory checks have bounded admission but can
wait behind filesystem work while body inactivity is suspended. Progressing trickle uploads
can retain all eight upload permits indefinitely. Immutable preflight does not consider a
hot rollback `-journal`, which gcs-mimic never creates in WAL mode but a foreign DELETE-mode
catalog could. Generation nonreuse is established for identities allocated by v3 from an
empty object catalog; accepted intermediate v3 catalogs do not reconstruct deleted history.

Every instruction-level fsync/deletion crash window, power loss, a permanently stalled
filesystem, native amd64, Windows, remote CI, real-cloud differential behavior, a 1 GiB
constrained-container transfer and RSS/cgroup/CPU/latency measurements remain unverified.
The next slice is object listing and metadata; JSON multipart follows separately.
