# Deterministic catalog lock release

Verified on 2026-10-21 with Rust 1.99.1 on aarch64 macOS. This fixes the remaining
reopen race after the earlier blocking-job ownership fix. No feature status,
canonical ledger or generated matrix was changed.

## Root cause and regression proof

The directory used an exclusive file lock released only by closing its descriptor.
An unrelated child process can inherit that descriptor between fork and exec,
keeping the same lock alive after the parent's last catalog handle drops. Parallel
catalog tests spawn child processes, exposing this window under workspace load.
Dropping the blocking job's Arc before publishing its result cannot fix that case.

`drop_releases_lock_while_a_forked_child_has_not_execed` holds a child in `pre_exec`
using a socket handshake, drops the parent catalog and immediately reopens it while
the child still holds the inherited descriptor. There are no retries or sleeps.
The handshake has a ten-second failure deadline. Added alone to the original
implementation, this test failed with `Unavailable("data directory <test-data-dir>
is already in use")`: **0 passed, 1 failed, exit 101**. See the
[actual pre-fix output](bucket-lock-release-before.txt). Only machine-local paths
were replaced with placeholders in recorded output.

## Fix and acceptance coverage

The directory lock guard explicitly unlocks before closing its descriptor. SQLite
still closes first; the lock file remains in place. Drop logs unlock errors;
explicit close returns SQLite close or unlock errors to its caller.

`Catalog::close(&self)` stops admission across all clones, waits for the execution
permit held by blocking work, and removes/closes the connection and directory lock.
It also handles a poisoned catalog. Old handles become unavailable, and repeated
close/drop cannot release a reopened catalog's independently acquired lock.

Server shutdown, signal errors, accept/task errors and drain timeout cleanup all
reach catalog close before `serve` returns. The binary's existing stopped event
follows successful `serve`, so it cannot precede catalog release.

| Test | Verified behavior |
| --- | --- |
| `drop_releases_lock_while_a_forked_child_has_not_execed` | Immediate reopen during a forced fork/exec overlap; failed before the fix |
| `close_waits_for_cancelled_blocking_work_and_invalidates_clones` | A channel holds a real blocking job after its async caller is aborted; one poll proves close remains pending and the directory stays locked until that job is released; stale close/drop leaves the new owner's lock intact |
| `repeated_concurrent_use_close_and_reopen_preserves_data` | 64 cycles, each with 16 concurrent reads, close/reopen, persistent metadata and stale-handle ownership checks |
| `shutdown_releases_catalog_before_return_with_state_clones_alive` | Server shutdown and an in-process restart release the directory before return while old and restarted State handles remain alive |
| `poisoned_catalog_permanently_revokes_readiness` | Existing poison/readiness checks plus successful close and immediate reopen after poison |

The existing immediate-drop restart, cross-process exclusion, committed-WAL
recovery and schema rejection tests remain unchanged and pass.

## Executed verification

All Cargo commands cleared `RUSTUP_TOOLCHAIN` and put the rustup proxies first in
PATH. Runtime tests used local endpoints only. No real Cloud Storage or metadata
server requests were made.

| Command | Result | Output |
| --- | --- | --- |
| `cargo fmt --check` | PASS, exit 0, no output | This report |
| `cargo clippy --workspace --locked --all-targets -- -D warnings` | PASS, exit 0 | [Clippy](bucket-lock-release-clippy.txt) |
| `cargo test --workspace --locked`, ten consecutive executions | PASS, 79 tests per run; 790 passes, 0 failures, 0 ignored | [All ten runs](bucket-lock-release-tests.txt) |
| `bash scripts/sdk-go.sh` | PASS, exit 0; go vet and both top-level Go tests, including the official SDK bucket lifecycle, no skips | [Go SDK](bucket-lock-release-sdk-go.txt) |
| `cargo xtask compatibility validate` and `cargo xtask compatibility check` | PASS, exit 0; 588 feature rows, 401 parameter locations, 357 schema fields | [Ledger](bucket-lock-release-ledger.txt) |
| `cargo xtask dependencies check` | Exit 1: Rust, crates and Go CURRENT; Rust builder digest UPDATE_AVAILABLE | [Dependency check](bucket-lock-release-dependencies.txt) |

The executed repetition loop was:

```sh
for run in {1..10}; do
  env -u RUSTUP_TOOLCHAIN cargo test --workspace --locked || exit "$?"
done
```

Each invocation used normal parallel test execution. The Go SDK harness also ran
concurrently with the repetition loop. No retry converted a failing invocation
into a passing run. An initial sandboxed workspace invocation could not bind local
sockets (`Operation not permitted`); all ten recorded runs had local socket access.

## Sources, ledger and limits

The following primary sources were checked on 2026-10-21:

- [Rust File locking and unlock](https://doc.rust-lang.org/std/fs/struct.File.html#method.unlock), documentation version 1.99.1.
- [Apple flock manual](https://developer.apple.com/library/archive/documentation/System/Conceptual/ManPages_iPhoneOS/man2/flock.2.html), including shared references inherited through fork.
- [Linux flock manual](https://man7.org/linux/man-pages/man2/flock.2.html), including explicit unlock versus closing every duplicate descriptor.
- [Tokio spawn_blocking](https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html), documentation version 1.54.0; started blocking work survives async cancellation.

This change affects the existing evidence for DESIGN-076, DESIGN-067, DESIGN-072
and UX-008/009. The integration owner retains the ledger; no status promotion is
needed for this correctness fix. No dependencies were added or pins changed.

Close intentionally waits for filesystem/SQLite work even beyond the network
drain deadline; it cannot cancel an already running blocking operation. Linux,
Windows, container rebuilds, real-cloud conformance and performance measurements
were not run in this task. The separate freshness check found a newer Rust builder
digest for both checked Rust image tags; that container update is outside this fix
and remains for the integration owner. There are no unresolved lock-release tests.
