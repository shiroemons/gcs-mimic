# JSON bucket CRUD integration evidence

Verified on 2026-10-21 on macOS arm64 with Docker
Desktop 29.8.1. Rust 1.99.1 (`58cf8f9c3`, 2026-10-13), edition 2024; Go 1.27.3,
cloud.google.com/go/storage v1.69.0 and google.golang.org/api v0.306.0.

## Commands and results

Cargo commands used `env -u RUSTUP_TOOLCHAIN` with the rustup proxy first in PATH
because the mise shim otherwise reintroduces Rust 1.93.0. Socket/network/container
checks ran with the sandbox permissions needed for loopback and Docker access.
Machine-local compiler paths in the retained logs are normalized to `.` or `./xtask`;
commands, versions and results are preserved.

| Command | Result | Log |
| --- | --- | --- |
| `cargo test --workspace --locked` | PASS, 70 tests; zero failed/ignored | [Rust](bucket-tests.txt) |
| `cargo fmt --all --check` | PASS, exit 0, no output | This report |
| `cargo clippy --workspace --locked --all-targets -- -D warnings` | PASS, exit 0 | [Clippy](bucket-clippy.txt) |
| `bash scripts/sdk-go.sh` with the rustup proxy first in PATH | PASS, vet and both Go tests; no integration skip | [Go SDK](bucket-sdk-go.txt) |
| `docker build -t gcs-mimic:dev .` | PASS, source build with bundled SQLite | [Guide](bucket-guide.txt) |
| `bash scripts/smoke-containers.sh linux/arm64 linux/amd64` | PASS, native arm64 and emulated amd64 | [Containers](bucket-containers.txt) |
| Getting-started shell blocks through the Go section | PASS, host/peer CRUD, restart and explicit reset, Go SDK on Compose | [Guide](bucket-guide.txt) |
| `cargo build --release --locked --bin gcs-mimic` plus guide native serve/probe/stop | PASS | [Native](bucket-native.txt) |
| `cargo xtask dependencies check` | PASS, all direct Rust/Go/Actions/container inputs CURRENT | [Dependencies](bucket-dependencies.txt) |
| `cargo xtask compatibility validate`, `render`, `check` | PASS, 588 rows and current generated English/Japanese matrices | [Ledger](bucket-ledger.txt) |

The guide's shell blocks were extracted with awk and executed together using
`bash -euxo pipefail -c`, preserving variables across blocks. The dedicated
`gcs-mimic-bucket-demo` project used allocated port 55030 and was removed with its
test volume after execution. Existing unrelated containers were untouched.
The smoke creates unique project/image/volume names and removes only its own data.
Its peer curl image is 8.23.0 at index digest
`sha256:11e64e28c5f58c8d93ee95fcf22ac055f48b0bc440e0ce7d8013bbb5ad6aa8ac`;
the dependency checker also verifies that pin and its current stable selector.

## Observable coverage

| Cases | Executed tests |
| --- | --- |
| BKT-INS-001/004, GET-001/002, DEL-001/002, ID-001/002, ERR-001 | `bkt_http_crud_metadata_conditions_and_errors` plus adapter identity/error units |
| BKT-ROUTE-001/002/003, QRY-001/002, INS-002/003, NAME-001/002, BODY-001 | `bkt_http_query_body_name_and_route_boundaries` plus exhaustive parameter/field/body adapter units |
| BKT-LST-001/002/003 | `bkt_http_sorted_scoped_pagination_and_limits`, `bkt_http_list_clamps_to_1000_with_lookahead` and token corruption/version units |
| Concurrent duplicate insert, process restart, exclusive directory ownership | `bkt_http_concurrent_duplicates_restart_and_exclusive_directory` (exactly one 200 and fifteen 409) |
| Failure before readiness | `bkt_http_failed_recovery_never_reports_ready` (corruption/path error) and catalog schema/lock units |
| Committed WAL recovery after abrupt process exit | `recovers_committed_wal_after_process_exit_without_drop` |
| Close/reopen ownership release, indexed queries, atomic conditions | Catalog integration tests and bounded-admission/cache unit test |
| Existing probe/resource/connection behavior | All eight foundation tests |

The six HTTP tests run the real binary, not a route mock. They cover exact 64 KiB
metadata acceptance, advertised-length and chunked oversize rejection, unknown
routes without body reads, malformed URI decoding, method/body errors, no cross-project
leakage, cursor insertion semantics, 1001 bucket lookahead, full/noAcl output and
200/1000 limits, GET 304 versus 412, DELETE rollback on failed conditions, persisted
metadata and delete across process restarts. Adapter units cover synthetic body
stream errors/trailers and impossible wall-clock ranges. No object exists, so
NotEmpty is only an error-mapping assertion, not an end-to-end deletion claim.

## Decisions and remaining limits

Bucket operation rows remain PARTIAL: no authentication, permission checks, ACLs,
ownership, billing, quotas, retention, soft delete or object side effects exist.
`projection=full` returns modeled fields and omits ACL/owner; this was explicitly
selected to preserve unchanged official high-level SDK calls. `projectNumber`
normalizes a decimal project or returns `"0"` for string IDs, a local simulation.
Unknown body fields are ignored under an unverified local rule; recognized
unsupported semantics return 501. GET failed not-match returns empty 304.
DELETE failed not-match returns 412, an explicitly unverified deviation from the
general preconditions table; see both official sources in the slice document.
ETags, cursor checksums, zero-size pages and naming edge cases retain the slice's
documented local limits.

Only bucket/catalog WAL recovery is implemented. Blob publication/reconciliation,
object streaming, fixtures, permission persistence, native amd64, remote CI/scheduled
execution, real-cloud differential checks and RSS/CPU/cgroup benchmarks are pending.
No real Cloud Storage or metadata endpoint was contacted by runtime tests. Online
freshness/builds fetched only release metadata, dependency artifacts and images.
The next slice is one bounded object media path with proven blob/catalog recovery.

## Ledger changes

Each changed row includes dated cases, test IDs and existing verification artifacts.
The required seed row UX-002 remains NOT_STARTED because no seeding exists.

| Previous → current status | Rows |
| --- | --- |
| NOT_STARTED → PARTIAL | `JSON-BUCKETS-001`, `JSON-BUCKETS-002`, `JSON-BUCKETS-005`, `JSON-BUCKETS-006`, `BEHAVIOR-COMMON-001`, `BEHAVIOR-COMMON-007`, `BEHAVIOR-COMMON-013`, `BEHAVIOR-BUCKET-001`, `DESIGN-072`, `DESIGN-082`, `UX-008`, `UX-009`, `DESIGN-055`, `DESIGN-063`, `DESIGN-071`, `UX-003`, `BEHAVIOR-COMMON-003`, `BEHAVIOR-COMMON-005`, `BEHAVIOR-COMMON-009`, `BEHAVIOR-COMMON-011`, `BEHAVIOR-COMMON-014`, `BEHAVIOR-BUCKET-002`, `BEHAVIOR-BUCKET-003`, `BEHAVIOR-BUCKET-004`, `UX-004` |
| NOT_STARTED → IMPLEMENTED | `DESIGN-076` |
| PARTIAL → PARTIAL | `DEP-005`, `DESIGN-067` |
| IMPLEMENTED → IMPLEMENTED | `DESIGN-058`, `DESIGN-060`, `DEP-001`, `DEP-003`, `DEP-008` |

API operations and behavior remain visibly partial; only directory ownership reaches
IMPLEMENTED in this slice. Location and storage-class achievement is METADATA_ONLY;
project/identity and supported local protocol subsets retain LOCAL_SIMULATION.

## Resolved failures during integration

The first merged catalog run failed immediate reopen with
`Unavailable("data directory ... is already in use")`. The blocking closure now
drops its captured catalog Arc before Tokio publishes job completion; the original
negative/immediate-reopen tests are retained and pass.

The first SDK run failed with `Error 501: Unsupported bucket API semantics:
projection=full` (45-second retry deadline). After the scoped projection decision,
the next run exposed `Error 400: Invalid pageToken for this project and prefix`
for the SDK's initial empty token. Both are fixed at request parsing; SDK requests
and malformed nonempty-token negative coverage are unchanged.

An initial peer smoke attempt used the Rust builder image, which has no curl
executable. The final smoke uses the pinned official curl image and passes on both
architectures. Restricted-sandbox freshness initially failed DNS resolution; the
explicit online run passed with network access. None of these failures was skipped
or counted as passing evidence.
