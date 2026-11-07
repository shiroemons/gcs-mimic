# Development and verification

<!-- markdownlint-configure-file {"MD013": false} -->

[日本語](../ja/development.md)

Requires rustup and curl for the explicit online freshness check. A host Python is not required: the Python SDK suite runs on the pinned Python that uv provides. Rust 1.99.1 is the verified latest stable compiler; edition 2024 is explicit. No Google credentials or project are needed.

## Development commands and push checks

Install [just](https://github.com/casey/just#installation) 1.56.0 or newer to use the checked-in
`Justfile`; `just` lists the available recipes. The recipes are intended for
macOS and Linux with Bash. The full pre-push check also requires Go, jq, curl,
the pinned Ruby/Bundler/gems from [the Ruby harness](../../sdk-tests/ruby/README.md),
Node.js 24+ with npm, uv 0.13.6+, the exact Java runtime and Maven in `sdk-tests/java/.java-version` and
`.maven-version`, the exact .NET SDK in `sdk-tests/dotnet/global.json`, Docker Engine/Desktop, Compose and Buildx. Docker must be running and able to
execute both Linux amd64 and arm64 containers, using emulation where needed.
The Justfile uses the standard [parallel attribute](https://just.systems/man/en/attributes.html)
and `--jobs` to control container concurrency.

```sh
just check
just check-sdk
just check-containers
just pre-push
```

Run `just pre-push` on the final source before every push. It runs three stages:

1. Rust and shell checks.
2. The complete Go, Ruby, Node.js, Python, PHP, Java, .NET and Rust SDK suites.
3. AMD64 and ARM64 container checks **in parallel**. Each builds its own image,
   then immediately runs its smoke tests without waiting for the other build.

A failed stage prevents the next stage from starting. The container stage waits
for both workers and their cleanup, then fails if either worker failed. Output
lines carry `[linux/amd64]` or `[linux/arm64]`, including build errors. The existing
harness isolates image tags, Compose projects, volumes and dynamically assigned
host ports for each worker.

`just pre-push` prints `[pre-push N/3 P%]` lines when each stage starts and
finishes, and every 30 seconds while a stage runs, with the elapsed time and an
estimate of the time left. Estimates use the stage durations of the previous
run, recorded under `target/pre-push-progress/`; the first run, or one after
`cargo clean`, reports only the stage count. Set
`GCSM_PRE_PUSH_PROGRESS_INTERVAL` to another number of seconds, or to `0` to
print only the stage transitions.

Run `just check` while editing and reserve the SDK/container checks for the final
source. Parallel builds use more CPU and memory; on a constrained Docker host,
use `just --jobs 1 pre-push` or `just --jobs 1 check-containers` to run them in
sequence. To investigate one architecture, use `just check-container linux/amd64`
or `just check-container linux/arm64`. The direct
`bash scripts/smoke-containers.sh` command still runs its requested platforms in
sequence. Dependency downloads and container pulls can require network access;
this is not an entirely offline command.

`just check` runs only the host's tests and the type-checks of the other targets, so
three kinds of failure reach CI first: tests whose result depends on timestamp
precision, Linux-only code, and the SDK suites in `just check-sdk-clients` /
`just check-sdk`. The precision differences have two sources. Linux and macOS both
let `SystemTime` hold nanoseconds, but the macOS clock only reads microseconds, so a
value from `SystemTime::now()` has a nanosecond part on Linux and not on macOS. On
Windows the `SystemTime` type itself counts 100 ns intervals and floors a finer
fraction.

Run the library tests on Linux with the pinned Rust image (`rust_image` in
`sdk-scorecard/run.sh`), keeping the build output in volumes so the host `target/` is
untouched. One container installs the build tools (the image has no `cc`), hands the
volumes to an ordinary user and runs the tests as that user:

```bash
cat > "$TMPDIR/linux-lib-tests.sh" <<'SCRIPT'
apt-get update -qq && apt-get install -y -qq build-essential pkg-config cmake protobuf-compiler
useradd -m tester
chown -R tester /t /usr/local/cargo/registry
su -m tester -c 'HOME=/tmp cargo test --locked --lib'
SCRIPT
docker run --rm -v "$PWD":/w -v gcs-target:/t -v gcs-cargo:/usr/local/cargo/registry \
  -v "$TMPDIR/linux-lib-tests.sh":/linux-lib-tests.sh:ro \
  -e CARGO_TARGET_DIR=/t -w /w "$RUST_IMAGE" sh /linux-lib-tests.sh
```

Do not run the tests as root: three permission-denied sync tests
(`ingest_unreadable_file_is_deferred_while_other_files_progress` and the two
`sync::mirror` permission tests) fail there, and CI does not run as root. Windows
tests cannot run locally; keep timestamp tests to values every platform holds exactly.

The local Rust check ignores an inherited `RUSTUP_TOOLCHAIN` and uses the
repository pin. The SDK recipe clears `GCS_MIMIC_GO_TEST_RUN` so an inherited
filter cannot silently omit tests. CI calls the same `scripts/check.sh` directly,
preserving its selected pinned/current-stable toolchain; CI does not require
just. `scripts/test-check.sh` verifies command order and failure propagation
without compiling or starting containers.
`just check` also runs `scripts/test-pre-push.sh`, which checks the actual
Justfile with stub harnesses: overlapping workers, prefixed output, completion
of both workers after failure, stage ordering and the serial fallback. Run it
directly with `bash scripts/test-pre-push.sh` after changing recipes. This local
Justfile test requires just; CI continues to use the direct shell harnesses and
separate native container jobs.

`scripts/test-rust.sh` runs every test binary and doctest that
`cargo test --workspace` runs. It runs the test binaries in a pool of
`GCSM_TEST_JOBS` (default 1), while the doctests run alongside them. It builds
the SDK server binary first: that build and the test build both re-link
`target/debug/gcs-mimic`, which the integration tests start, so the two must
not overlap with running tests. `just check` and `just check-sdk` run through
`scripts/with-test-tmpdir.sh`. By default the tests use the normal `TMPDIR`
with a serial pool, the same conditions as CI. Tests keep their real file
synchronization. macOS implements it with `F_FULLFSYNC`, which flushes the
system disk and dominates test time there; concurrent tests on the system disk
delay processes enough to break wall-clock deadline tests, so the pool stays
serial. On macOS, `GCSM_TEST_RAMDISK=1` opts in to a temporary RAM disk
(`GCSM_TEST_RAMDISK_MB`, default 4096) used as `TMPDIR`, with the default pool
raised to 4. The RAM disk is not the default because it pins memory: a disk
that fails to detach keeps that memory until it is detached by hand and can
destabilize the whole machine, while the default only costs time. On exit the
wrapper stops processes that still hold files on the disk (with `SIGTERM`, then
`SIGKILL`, listing each one) before detaching it: a busy disk fails a plain
detach, and `-force`, kept as the fallback, would unmount files still in use.
Every macOS run also releases test RAM
disks left by a run that was killed before its cleanup, identified by the
wrapper PID in the volume name; a disk whose PID is still alive is left alone.
If the RAM disk cannot be created, the wrapper warns and continues serially. On
other platforms the wrapper changes nothing. A timing-sensitive failure can
appear with or without the RAM disk; treat it as a test defect either way. These settings use the `GCSM_TEST_` prefix
because the server reads every `GCS_MIMIC_` and `FAKE_GCS_` variable as its own
setting and rejects unknown names.

`bash scripts/sdk-go.sh` alone runs only the Go suite. CI's `sdk-go` job passes
`--with-clients` (Go plus the Java, .NET and Rust clients), and a separate
`testcontainers` job builds the image once and runs the Java, .NET and Node.js
Testcontainers adapters against it. The SDK recipe explicitly passes
`--with-ruby --with-node --with-python --with-php --with-java --with-dotnet --with-rust`. Each flag
runs that language's official SDK suite against the same permissive and strict
servers. Every suite runs its origin guard self-check, a migration scenario,
the permissive journeys, and the strict journeys with authorization denials and
signed URLs. The Ruby suite requires the installed pinned gems and fails if they
are absent. [The Node.js suite](../../sdk-tests/node/README.md) needs Node.js 24+
with npm and installs its lockfile with `npm ci --ignore-scripts`.
[The Python suite](../../sdk-tests/python/README.md) needs uv, which provides the
pinned Python. [The PHP suite](../../sdk-tests/php/README.md) runs in the
digest-pinned Composer/PHP image and needs Docker, not a host PHP.
[The Java](../../sdk-tests/java/README.md), [.NET](../../sdk-tests/dotnet/README.md)
and [Rust](../../sdk-tests/rust/README.md) suites use host toolchains at their
exact pins. Each README lists the journeys that its SDK cannot express.
`GCS_MIMIC_SDK_SCHEME=https just check-sdk` verifies Go, Ruby, Node.js, Python
and PHP with the exported local CA. The Java, .NET and Rust suites guard a plain
loopback HTTP origin only, so the HTTPS run prints them as `SKIPPED` instead of
running them. Signature endpoint/payload caveats are
recorded in [the signed-access evidence](evidence/signed-urls.md).

No Git hook is installed and no commit or push is performed. Re-run the checks
after changes. A local pass does not establish native amd64 or GitHub Actions
success: the configured remote lanes must also pass after pushing.

The Foundation checks workflow restores Cargo dependency builds with
`Swatinem/rust-cache`, and only runs on `main` save them, so branches and pull
requests reuse the latest `main` cache. A newer push cancels the previous run
for the same branch or pull request; `main` keeps every run. Windows tests use
`RUNNER_TEMP` as their temporary directory. The release workflow's native test
runs use the same setting but no Cargo cache, so release archives always build
from a clean target directory.

Debug and test builds keep only line tables (`debug = "line-tables-only"` in
the workspace `[profile.dev]`), which keeps panic and backtrace locations while
shortening compiling and linking and shrinking `target/`. Dependencies build
with `opt-level = 3` (`[profile.dev.package."*"]`) because the tests spend most
of their CPU time in them; the library tests drop from about 121 s to 79 s, and
the dependencies compile once and stay cached. Keep Cargo's default
incremental compilation for local work: after a one-line change in `src/`,
`cargo test --workspace --no-run` rebuilds in about 8 s instead of about 43 s
with `CARGO_INCREMENTAL=0` (Apple M-series, 14 cores). Cargo never deletes
incremental state left by an earlier compiler, flag or feature set, so
`just check` first runs `scripts/trim-target.sh` (also `just trim-target`). It
removes incremental crate directories no build touched for
`GCSM_TARGET_INCREMENTAL_DAYS` (default 3) and, if `target/` still exceeds
`GCSM_TARGET_MAX_MB` (default 30720), all incremental state and then the debug
and cross-check outputs. Cargo turns incremental compilation off by default
when it detects CI.

Changes that touch only Markdown files or `docs/` skip Foundation checks and
run the Documentation checks workflow instead. It runs `scripts/check-docs.sh`,
the same documentation checks that `scripts/check.sh` runs: the feature ledger
and generated matrices, release-note links and the Docker Hub overview. Add any
new check that reads committed documentation to that script so both workflows
run it. A change that also touches other files runs Foundation checks as usual,
and release workflows always run the full checks. Use manual dispatch to run
Foundation checks for a documentation-only commit.

Markdown paragraphs are not hard-wrapped: write one paragraph or list item per line, which is why the root `.markdownlint-cli2.jsonc` disables MD013 (line length).

Run the separate online freshness check at slice start and before release:

```sh
just check-dependencies
```

Freshness remains a separate gate because it checks changing upstream metadata,
while `pre-push` validates the working tree and locked inputs. An update or a
network failure must be reported; it is not a passing freshness result.

## Rust lint policy

The root `Cargo.toml` defines `unsafe_code = "deny"` in
`[workspace.lints.rust]` and `undocumented_unsafe_blocks = "deny"` in
`[workspace.lints.clippy]`. The application and `xtask` opt in with
`[lints] workspace = true`; add that opt-in to every new workspace member.
The policy covers each member's library, binaries and tests. `just check` runs
workspace/all-target Clippy with `-D warnings` in addition to these manifest lints.
It also type-checks the Windows MSVC, Linux musl and macOS ARM64 release targets,
plus the Linux glibc target that the CI Rust jobs use, other than the host
through `scripts/check-cross.sh`, installing missing Rust targets with `rustup`.
Foundation CI runs that script in a separate `cross` job beside the `rust` job,
which sets `GCSM_CHECK_CROSS=0` to skip it in `scripts/check.sh`.
The glibc target is needed because std gives Linux glibc types such as
`fs::Metadata` extra statx fields, so size lints such as `large_enum_variant`
fire only there. C dependencies compile to empty stub objects there, so
this catches `cfg` and platform-API mistakes but not link or runtime failures.
Foundation CI's `portability` job runs the tests natively on one target per
OS family on every push; the release matrix still runs only for release tags
and manual dispatch. Release workflows skip `portability` because their `test`
matrix already tests every release target natively.

Tests of deadlines and timeouts must not depend on how fast a shared CI runner
is. Timestamps come from the injected `Clock` (`FixedClock`, `TestClock`), but
request, inactivity and catalog-wait deadlines use Tokio time. Test their
selection logic in virtual time with `#[tokio::test(start_paused = true)]` and
`tokio::time::advance`, as the `await_session_upload` tests do. Let the task
reach its wait before advancing, and check completion before awaiting it: an
idle paused runtime auto-advances to the next pending deadline.

Do not pause time in tests that move data over real sockets. Tokio
auto-advances whenever the runtime waits on the kernel, for example during TCP
flow control, so a 32 MiB transfer can expire a 5 s inactivity deadline in
virtual time. Blocking catalog work inhibits auto-advance instead, so a catalog
wait never elapses on its own.

When a test must use wall-clock time against a real server, hold the wait open
with a lock or another explicit gate rather than a sleep, keep each activity gap
at most a quarter of the timeout it must not trigger, and keep inactivity
timeouts at 500 ms or more. The inactivity timeout also bounds each request's
header read, setup requests included; at 100 ms, CPU starvation on a loaded
host closed connections before their headers were dispatched. Assert the observed close time rather than the client's
remaining sleeps, accept `BrokenPipe`, `ConnectionReset` and
`ConnectionAborted` as the server closing first, retry 503 responses from setup
steps that run under a short catalog budget, and use client socket timeouts and
retry deadlines only as hang guards.

Keep `#[expect(unsafe_code, reason = "...")]` on each necessary unsafe statement
or its binding, with an adjacent `// SAFETY:` comment explaining why the
operation is safe; production code uses `#[expect]` rather than `#[allow]`
(see the production lint gate below). Clippy rejects unsafe blocks without
these comments. Callbacks that
convert raw pointer arguments to references must be `unsafe extern "C" fn` with
a `# Safety` contract. Their item-level expectation is needed for the unsafe declaration
but also permits unsafe code throughout the body and nested items; it cannot be
limited to the declaration. Keep body operations in explicit inner unsafe blocks
with `// SAFETY:` comments. Edition 2024's `unsafe_op_in_unsafe_fn` and Clippy's
`undocumented_unsafe_blocks` still check those bodies under `-D warnings`.
Review every unsafe operation and nested item within that allowance for necessity
and safety; these lints do not establish either.
Existing exceptions cover Windows publication and console control, SQLite test
hooks and the Unix pre-exec test. Do not allow unsafe for an entire crate or test
module. Cross-target checks must include Windows: native Unix checks do not
compile its `cfg(windows)` branches.

`src/fsutil.rs` holds descriptor-based filesystem operations. Keep new
OS-specific unsafe calls there, each with a narrow
`#[expect(unsafe_code, reason = "...")]` and a `// SAFETY:` comment that
documents pointer/handle ownership and platform preconditions, and verify all
affected platforms. Do not add an exception before it is needed.

Every new input-parsing module must start with this inner attribute:

```rust
#![deny(clippy::unwrap_used, clippy::expect_used)]
```

Alternatively, put `#[deny(clippy::unwrap_used, clippy::expect_used)]` on its
`mod` declaration. Use fallible parsing and propagate errors. These restrictions
are module scoped, not workspace-wide: existing unwrap/expect uses are outside
this slice. Where a parsing module's tests need these assertion helpers, place
an explicit local `#[allow(clippy::unwrap_used, clippy::expect_used)]` on its test
module with a reason; do not relax the production parser.

When reviewing every subsequent configuration, initializer or import slice,
check that each new parsing module has the deny attribute on the module or its
declaration, and that test exceptions do not cover production code. Run the
unwrap/expect rejection probe when a module boundary or lint scope changes.
This is a required review check: the current module structure does not enforce
the opt-in automatically. A shared parser parent is deferred until real parser
modules justify it; do not move legacy parsers solely to centralize the lint.

The [repository lint contract](slices/repo-lints.md) defines acceptance and scope.
Cargo's [lint manifest reference](https://doc.rust-lang.org/cargo/reference/manifest.html#the-lints-section)
and the [Rust lint-level reference](https://doc.rust-lang.org/rustc/lints/levels.html)
describe inheritance and local overrides.

The workspace lints also deny `clippy::dbg_macro`, `clippy::todo` and
`clippy::unimplemented`, so debugging leftovers and placeholders fail the
`-D warnings` run, and `unreachable_pub` and `clippy::redundant_clone` on
every target.

### Production lint gate

`cargo xtask lint` enforces the Rust implementation guardrails in
[AGENTS.md](../../AGENTS.md#rust-implementation-guardrails). `scripts/check.sh`
runs it after workspace Clippy, so `just check` and Foundation CI run it too.
It prints every rejected occurrence in server production code. Existing rules
remain zero tolerance. Indexing, string slicing, potentially truncating or
sign-losing casts and static-string errors use per-file baseline counts;
no new finding may increase those counts. Text rules share
`xtask/lint-baseline.txt`; Clippy uses `xtask/lint-baseline.<triple>.txt`.
The other exceptions are:

- a reasoned `#[expect(<lint>, reason = "...")]` for a Clippy or rustc lint
  (see [below](#expect-for-restriction-lints));
- a reasoned `// xtask-lint: allow(text::<rule>) -- <reason>` comment for a
  text rule (see [Reasoned suppressions](#reasoned-suppressions));
- exactly one definition of `fn unavailable`, the shared storage error
  mapping, in `src/storage/failure.rs`. A definition anywhere else fails, and
  so does a second one in that file or a missing one.

The two `#[path]` attributes in `tests/common` are test-only exceptions that keep
fixtures shared with unit tests in a single source.

The gate prints how many text-rule findings suppression comments silenced,
per rule, and fails on any invalid or stale comment; the compiler checks
`#[expect]` itself.

The gate covers only server production code: files under `src/` except
`src/grpc/generated/`, the `src/grpc/codegen/` build tool, `*_tests.rs`,
`tests.rs`, `test_support.rs` and files inside a `tests/` or `*_tests/`
module directory. Clippy runs on the `gcs-mimic` library and binary only
(`--lib --bins`), so `cfg(test)` code is not compiled; the text rules skip
each item marked `#[cfg(test)]` or `#[cfg(all(test, ...))]`, including
attributes stacked before that `cfg`. Tests may unwrap and expect.

`cargo xtask lint --target <triple>` runs only the Clippy pass for another
target, so code behind that target's `cfg` (for example Windows-only file
naming) is checked; the text rules do not depend on the target and run with
the host gate. `scripts/check-cross.sh` runs it after the cross Clippy run for
each target in its list, with the same C compiler stub and target directory.

#### Rules

Prefer a Clippy lint or a `clippy.toml` entry to a new text rule: Clippy
resolves types and paths, so it cannot be fooled by naming, and its
`#[expect]` already checks staleness. The text rules cover only what Clippy
cannot express.

| Rule | Rejects |
| --- | --- |
| `clippy::unwrap_used`, `clippy::expect_used`, `clippy::panic` | Panics reachable from requests. |
| `clippy::too_many_lines` | Functions over 120 lines (`clippy.toml`). |
| `clippy::fn_params_excessive_bools` | Functions with any `bool` parameter (`max-fn-params-bools = 0`). |
| `clippy::needless_pass_by_value` | Avoidable copies. |
| `clippy::allow_attributes_without_reason` | `#[allow]` or `#[expect]` without a `reason`. |
| `clippy::allow_attributes` | Any `#[allow]`; use `#[expect]`, which fails once the lint stops firing. |
| `clippy::disallowed_methods` | The methods listed in `clippy.toml`: unsupervised spawns (`tokio::spawn`, `tokio::task::spawn`, `tokio::task::spawn_local`, `tokio::task::spawn_blocking`, `Handle::spawn_blocking`, `Runtime::spawn_blocking`, `Handle::spawn`, `Runtime::spawn`, `std::thread::spawn`, `std::thread::Builder::spawn`) outside `src/task.rs`, and rusqlite's `Connection::execute`, `query_row`, `query_one` and `prepare`, which prepare their SQL on every call. Calls through a `Transaction` resolve to the `Connection` methods; `Statement` methods from `prepare_cached`, `execute_batch`, `JoinSet::spawn` and scoped threads are not listed. |
| `clippy::indexing_slicing`, `clippy::string_slice` | Potential panics from indexing and slicing; per-file ratchet. |
| `clippy::cast_possible_truncation`, `clippy::cast_sign_loss` | Silent truncation and sign loss; per-file ratchet. Casts do not themselves panic. |
| `text::path_attribute` | `#[path]` module attributes. |
| `text::string_error_result` | `fn` signatures and type aliases of `Result<_, String>`, plus associated `type Err = String` and `type Error = String`. |
| `text::static_str_error` | `Result<_, &'static str>` in signatures/aliases and associated static-string error types; per-file ratchet. |
| `text::string_error_variant` | Tuple variants `Backend(String)`, `BodyAborted(String)`, `Database(String)`, `Internal(String)`, `Io(String)`, `Other(String)`, `Sqlite(String)`, `Storage(String)` or `Unavailable(String)` in an enum named `*Error`. |
| `text::stringified_error` | `map_err(\|e\| Path(e.to_string()))` or `map_err(\|e\| Path(format!(".. {e}")))` whose only argument stringifies the error. |
| `text::unavailable_helper` | `fn unavailable` outside `src/storage/failure.rs`, or not exactly one there. |
| `text::allow_too_many_arguments` | `#[allow]`, `#[expect]` or `cfg_attr` of `clippy::too_many_arguments`, including attributes split over several lines. |
| `text::format_sql` | `format!` whose literal starts with an SQL statement. |
| `text::sync_without_flush` | `.sync_all().await` or `.sync_data().await` after an awaited write in the same function with no `.flush().await` or `.shutdown().await` in between. |

`unreachable_pub` and `clippy::redundant_clone` pass on every workspace target,
so `[workspace.lints]` denies them and the plain
`cargo clippy --workspace --all-targets -- -D warnings` run enforces them,
tests included. The gate does not pass them with `-W`: a command-line `-W`
would lower that `deny` to a warning. `[workspace.lints]` instead sets
`clippy::disallowed_methods` to `allow`, so tests may spawn tasks and run
ad-hoc SQL, and the gate's `-W` raises it for production code only. A unit
test in `xtask` fails if a lint is denied there and also passed with `-W`.

The text rules ignore comments and string contents. They are heuristics:
for example, `text::format_sql` does not see SQL assembled through other
helpers, and a `Result<_, String>` hidden behind another alias is not counted.
`text::sync_without_flush` follows source order without tracking which value
was written, so it misses a write and a sync in different functions and
reports a socket write followed by a file sync. `text::string_error_variant`
checks only the listed variant names, so a variant that names rejected client
input, such as `InvalidTopic(String)`, may keep its message. Review still
applies the written rules where the tool cannot.

New error types must not be `&'static str`, even for rejected client input:
strings do not implement `Error` and cannot preserve a cause chain. Use a
typed error with `Display` and `Error`; it may hold a static message.
The unused `http-body-util` channel feature is disabled. Producer-driven
response bodies use `server::chunk_body` to preserve the final queued frame.

#### Per-file ratchet

Each baseline row is `rule`, repository-relative file and positive count,
separated by tabs. Blank lines and `#` comments are ignored. Counts belong to
files rather than line numbers, so moving a line does not require an update.
Unknown rules, duplicate pairs and malformed counts fail parsing.

Every gate fails increases. The compiler pinned by `rust-toolchain.toml`
also rejects stale Clippy counts: lower a count as soon as a finding is fixed,
deleting the row at zero. Removed files are stale too. Other compiler releases
and `RUSTUP_TOOLCHAIN=stable` (including its host-qualified spelling) warn on
Clippy decreases, because their diagnostics can differ from the pin. Shared
text counts always reject staleness. CI retains pinned and current-stable lanes.
The host gate selects the native compiler's triple and the shared text file;
`--target` selects only that triple's Clippy file. Target-specific `cfg` counts
cannot hide another target's regressions or create stale host entries.
It omits `--target` to reuse native build artifacts; run the host gate with
Cargo's native default target, and use `--target TRIPLE` for cross builds.
Reasoned Clippy expectations and text suppressions are excluded
from the counts and remain subject to their existing staleness checks.

`cargo xtask lint --baseline` prints sorted shared text counts to stdout;
`--baseline --target TRIPLE` prints only that triple's Clippy counts. Neither
mode writes files. Establish each reviewed baseline once after integration:

```sh
cargo xtask lint --baseline > xtask/lint-baseline.txt
cargo xtask lint --baseline --target aarch64-apple-darwin > xtask/lint-baseline.aarch64-apple-darwin.txt
cargo xtask lint
```

Repeat the target command for `x86_64-pc-windows-msvc`,
`x86_64-unknown-linux-gnu` and `x86_64-unknown-linux-musl`, matching
`scripts/check-cross.sh` and CI. Use its C compiler stub and target directory
when the target cannot build natively. Do not regenerate unchanged baselines
to hide a new finding; review and lower individual entries when fixing them.

Committed Clippy baselines cover macOS ARM64, Windows MSVC, Linux GNU x86-64
and Linux musl x86-64. A different host still runs Clippy and the shared text
rules: a missing target file is an empty baseline, so every ratcheted finding
fails as new. Establish its file with `--baseline --target TRIPLE` after
reviewing those findings; no other target's counts are substituted. The shared
text baseline is required. Review increases explicitly; ordinary fixes should
only lower counts.

#### Reasoned suppressions

A Clippy or rustc finding that the rule's intent does not cover takes
`#[expect(<lint>, reason = "...")]` on the narrowest item that holds it,
usually the function. The compiler fails an expectation that no longer
fires, so a stale exception cannot linger.

Statements that no API request reaches are the intended exception to the
rusqlite entries of `clippy::disallowed_methods`: those that run at open,
during a schema migration, at startup or only in a background worker. A
request reaches a statement indirectly too, through a wakeup it triggers.
Resumable session, rewrite and derived-transfer requests wake the maintenance
sweep, so the sweep's statements use `prepare_cached` like any request path.
A function that runs only at open carries the reason itself:

```rust
#[expect(clippy::disallowed_methods, reason = "crash recovery runs once at open")]
fn recover_on_open(connection: &mut Connection) -> Result<(), CatalogError> {
```

A module that no request path calls, such as the sync mirror's write-back
state, uses one inner `#![expect(...)]` whose reason names what makes it
request-free. Module scope also hides a request-path statement added later,
so trace the callers before adding a statement to such a module, and move
the expectation to its functions once any caller is request-driven.

A text-rule hit that the rule's intent does not cover, such as a schema
migration whose DDL is built only from static fragments, can be suppressed
with a line comment:

```rust
// xtask-lint: allow(text::format_sql) -- table names come from a fixed list
let statement = format!("CREATE TABLE {table} (id INTEGER PRIMARY KEY)");
let sql = format!("SELECT {COLUMNS} FROM objects"); // xtask-lint: allow(text::format_sql) -- static column list
```

- A comment after code covers its own line; a comment on its own line covers
  the next line with code. Several rules may be listed, separated by commas.
- The reason after ` -- ` is mandatory and at least 10 characters long.
- Only `text::` rules are accepted; an unknown name is an error. Clippy and
  rustc lints use `#[expect(<lint>, reason = "...")]`, which the compiler
  already checks.
- A listed rule that reports nothing on the covered line is a stale
  suppression and fails the check, like an unfulfilled `#[expect]`. Remove the
  comment when the code no longer needs it.
- Doc comments and string contents never suppress anything.

#### `#[expect]` for restriction lints

`clippy::unwrap_used` and `clippy::expect_used` are allowed by default, and
the plain `cargo clippy --workspace --all-targets -- -D warnings` run does not
enable them. An `#[expect(clippy::expect_used, reason = "...")]` is still
checked there: the attribute sets the lint's level inside the item, so Clippy
runs the lint in that scope. A matching call fulfills the expectation; without
one, `unfulfilled_lint_expectations` fails the `-D warnings` run. This was
verified on Rust 1.99.1 with a scratch crate. `cargo check` and `cargo build`
do not run Clippy lints, so they report neither case. The gate's `-W` run
does not count a call covered by a fulfilled `#[expect]`.

Code that only unit tests use is compiled under the same `cfg` as those
tests, for example `#[cfg(all(test, unix))]`, instead of staying in production
builds as dead code. A field that only tests read keeps a reasoned
`#[cfg_attr(not(test), expect(dead_code, reason = "..."))]`.

#### Moving a lint to every target

A gate lint that passes on every workspace target, which
`cargo clippy --workspace --all-targets -- -W <lint>` shows, can move to
`[workspace.lints]`. `clippy.toml`'s `allow-unwrap-in-tests`,
`allow-expect-in-tests` and `allow-panic-in-tests` cover `#[test]` functions
and `#[cfg(test)]` modules, but not helper functions of integration tests.

1. Add it to `Cargo.toml`, for example `unwrap_used = "deny"` (without the
   `clippy::` prefix) under `[workspace.lints.clippy]`.
2. Remove it from `CLIPPY_LINTS` in `xtask/src/lint.rs`.
3. Run `just check`.

### Waiting in tests

Shared CI runners stall for seconds, so a test that sleeps for a fixed time,
or polls against its own short deadline, passes or fails with machine speed.
Wait for the state instead, through the helpers in `src/test_support.rs`:
`eventually` and `eventually_some` block, `eventually_async` and
`eventually_some_async` await. They poll every 10 ms and fail only after
`HANG_GUARD` (60 s), which guards against a hang and is never a latency bound.
Integration tests reach them as `common::eventually` and
`common::input_fixtures::*`. The async helpers are not for paused-time tests,
whose clock jumps over the guard while blocking work runs.

Bound elapsed time only to tell two mechanisms apart, and keep the competing
deadline far from the bound: for example, a test that proves an early rejection
does not wait for the body sets a 30 s idle deadline and asserts less than 10 s.
Lower bounds (`elapsed() >= ...`) are safe, because a slow runner only lengthens
them.

`cargo xtask lint` counts `sleep(` and `Instant::now()` calls in test files
under `src/` and `tests/` (outside `src/test_support.rs`) against
`xtask/test-clock-baseline.txt`. A file may not gain a call; a file that loses
one must lower its entry. Regenerate the whole list with
`cargo xtask lint --test-clock-baseline > xtask/test-clock-baseline.txt` and
review the diff: only lower counts are expected.

`just stress-tests` (`scripts/stress-tests.sh`) runs the Rust tests repeatedly
with parallel test binaries while busy loops occupy every CPU;
`GCSM_STRESS_ROUNDS` (default 2) and `GCSM_STRESS_JOBS` (default: CPU count)
adjust it. The daily **Timing stress** workflow runs it on Linux and macOS. A
failure there names a timing-dependent test to make deterministic, not a product
regression.

## Protobuf generation checks

Foundation CI checks `src/grpc/proto/SHA256SUMS` and exercises the offline
hash/drift rejection fixture. Daily dependency CI installs the digest-pinned
compiler from `src/grpc/codegen/protoc.json`, regenerates v1 then v2 into a
temporary directory and compares it with committed generated Rust. It also
compares the compiler pin with the latest stable official protobuf release.
Generation and compiler checks use `!cancelled()` alongside the SDK fetch and
dependency detector, so a failed compiler check does not skip dependency detection.
Each failed step still fails the job; cancellation stops the remaining checks.
Ordinary application and release builds still require no protoc.

```sh
bash scripts/test-grpc-codegen.sh
CARGO_BUILD_JOBS=4 bash scripts/check-grpc-codegen.sh
cargo run --locked --offline -p grpc-codegen -- --out-dir <output-directory>
```

Use protoc 36.2, whose version and Linux/macOS archive SHA-256 values were verified
against the [official release](https://github.com/protocolbuffers/protobuf/releases/tag/v36.2)
on 2026-11-04. Update the pin and regenerate when daily freshness detects a newer
stable compiler. This checks local source integrity and regeneration drift;
automatic discovery of upstream API schema changes remains pending.

## Direct verification commands

Native archive preparation and the release CI matrix are documented in the
[binary distribution guide](binary-distribution.md). The local `just pre-push`
recipe does not replace Windows/macOS Intel/native Linux matrix execution.
The [GHCR publication procedure](container-distribution.md) follows a successful
tag-driven binary Release. `just check` runs its offline publication-policy
tests; it does not authenticate to or write to a registry.

```sh
rustup toolchain install 1.99.1 --profile minimal --component rustfmt --component clippy
cargo +1.99.1 fetch --locked
cargo +1.99.1 xtask dependencies check
cargo +1.99.1 xtask compatibility validate
cargo +1.99.1 xtask compatibility render
cargo +1.99.1 xtask compatibility check
cargo +1.99.1 test --workspace --locked --offline
cargo +1.99.1 fmt --all --check
cargo +1.99.1 clippy --workspace --all-targets --locked --offline -- -D warnings
cargo +1.99.1 xtask lint
bash scripts/smoke-containers.sh
bash scripts/sdk-go.sh
```

The xtask alias uses locked offline resolution. Fetch all locked packages first. Explicit `+1.99.1` overrides a conflicting `RUSTUP_TOOLCHAIN` environment variable.

Latest verified direct releases: Tokio 1.54.1, Hyper 1.12.0, hyper-util 0.1.21, http-body-util 0.1.5, serde_json 1.0.154 and rusqlite 0.41.1. Versions, licenses, sizes and sources are in the [dependency snapshot](../../compatibility/dependency-status.json). Cargo.lock records the full graph. Docker inputs, including the test-only peer curl image, use actual multiarch digests. The snapshot checks both Dockerfile and smoke-script pins.

The object media slice additionally uses bytes 1.12.1, md-5 0.11.0, crc32c 0.6.9 and
getrandom 0.4.3, with the selection reasons and package sizes in the
[dependency policy](dependency-policy.md#dependency-status-evidence).

The Go harness requires Go and jq; it pins Go 1.27.3, storage v1.70.0 and API v0.311.0 with committed go.mod/go.sum. Its high-level HTTP JSON bucket and object listing/Update/Attrs/NewReader/Delete workflows run against an isolated native server; the getting-started guide verifies the same suite against Compose. Raw media upload uses the harness HTTP client; SDK Writer with `ChunkSize=0` exercises multipart metadata, hashes and conditioned publication. The default resumable Writer runs through the JSON resumable session engine (`objects_resumable_test.go`). Media/multipart upload and JSON copy tokens use bounded persistent response replay with or without generation conditions ([replay contract](slices/sdk-upload-replay.md)); GET/DELETE/metadata-update tokens retain their existing acceptance without deduplication (SPEC-013/SPEC-015). `scripts/sdk-go.sh` also starts a second isolated native server with `--auth-mode strict --identity-file config/identities.json`, checks that its ready event reports strict mode, and runs the `TestStrictAuth*` suite against it through `GCS_MIMIC_STRICT_ENDPOINT`. Those clients use an explicit endpoint and an `oauth2.StaticTokenSource` with the public fixture tokens, so no Application Default Credentials or metadata-server lookup occurs; the suite skips only when that variable is unset. Rust `auth_http` and `url_cli` tests cover strict authentication, role enforcement, startup validation and the URL helper offline. See the [strict authentication evidence](evidence/strict-auth.md). Tests make no Google/metadata-server requests. Rust integration tests use isolated temporary catalog directories, including startup failure, generations, condition races, restart and selected SIGKILL publication boundaries. On unix, unit tests also take power-loss snapshots of single-request uploads, resumable finalization and compose/copy/rewrite publication that keep only the file lengths and directory entries the seamed sync points made durable (model and limits in `src/storage/catalog/fault.rs`), so dropping the blob or shard fsync fails them. Renames, unlinks and new-file entries outside the seam, such as recovery's restore-to-staging renames, are not modeled. Every other fsync/deletion instruction window, real power loss and memory/CPU/latency measurements remain unverified. See the [multipart integration evidence](evidence/multipart.md) and [guide transcript](evidence/multipart-guide.txt).

`dependencies check` contacts official Rust, Cargo, GitHub, Debian, Docker Hub and
public GCR metadata. Other Rust tests are offline and use only loopback. All direct
dependencies are compared against non-yanked stable releases, including new majors;
Docker tags/digests and Debian stable codename are checked separately. Exit 1 means
updates; exit 2 means verification failed. `--scope release` compares only the inputs
that build or ship release artifacts, as the release workflow does; see the
[dependency policy](dependency-policy.md#ci-lanes). Run at slice start and before release
and review exceptions weekly. No manifests are modified and nothing is published
automatically.

Pinned/current-stable Rust CI, cross-target checks and native amd64/arm64 container CI are configured. Daily freshness runs at 03:17 UTC every calendar day without cooldown after reaching the default branch; scheduled runs have been observed since 2026-10-30, and a run that finds an update fails until the update is adopted. Latest-dependency candidate builds and upgrade submission are not implemented. Local arm64 runs natively; local amd64 uses emulation.

`compatibility/features.json` is the only implementation-status authority. English and Japanese views are generated together under `docs/en` and `docs/ja`. Japanese user documentation starts at [docs/ja](../ja/README.md); historical planning material remains under [planning/](../../planning/README.md). Ledger tooling validates evidence structure and links, not the truth of submitted logs. No GCS behavior is claimed by internal probes.

- [Design and extension boundaries](design.md)
- [Roadmap](roadmap.md)
- [Compatibility policy](compatibility-policy.md)
- [Generated English matrix](feature-matrix.md)
- [Generated Japanese matrix](../ja/feature-matrix.md)
- [API inventory](api-inventory.md)
- [Sources and gaps](sources.md)
- [Dependency policy](dependency-policy.md)
- [Future onboarding contract](getting-started-spec.md)
- [Tests](evidence/p0-tests.txt)
- [Container checks](evidence/p0-containers.txt)
- [Freshness check](evidence/p0-dependencies.txt)
- [Coding-agent rules](../../AGENTS.md)

## Differential harness

`cargo xtask differential` sends the same declarative request sequences to the
real Cloud Storage service and to a freshly built native-profile `gcs-mimic`,
normalizes both responses and reports every difference in status, headers and
JSON body paths. It finds drift in implemented behavior; it does not decide
which side is right, and it never fixes anything. HTTP goes through the
external `curl` command and gRPC through the external `grpcurl` command, both
behind one transport trait.

The local server uses a loopback Pub/Sub stand-in that returns 404 for the
notification case's deliberately missing topic. Notification creation therefore
exercises gcs-mimic's configured-emulator existence check; it never contacts
Google Pub/Sub. Other topic lookups and publish requests are refused by the
stand-in.

```sh
cargo xtask differential validate            # parse and check the case files, no network
cargo xtask differential run                 # self-check: gcs-mimic only
cargo xtask differential run --reference <recording.json>
GCS_MIMIC_DIFF_PROJECT=<project> GCS_MIMIC_DIFF_TOKEN="$(gcloud auth print-access-token)" \
  cargo xtask differential run --record <recording.json>
# after an interrupted live run: delete the leftover gcsmimic-diff-* buckets
GCS_MIMIC_DIFF_PROJECT=<project> GCS_MIMIC_DIFF_TOKEN="$(gcloud auth print-access-token)" \
  cargo xtask differential sweep
```

Modes:

- **Self-check** is the default. Nothing contacts Google. It checks that every
  case runs and that each step meets its `expect_status`.
- **Live** needs both `GCS_MIMIC_DIFF_PROJECT` and `GCS_MIMIC_DIFF_TOKEN`
  (an OAuth access token); one alone is an error. The token goes to `curl` on
  stdin, never on the command line. Every bucket is private and disposable,
  named `gcsmimic-diff-<run>-...`, with public access prevention `enforced` and
  labelled `gcs-mimic-differential=1`. The shared bucket (`-b`) and the bucket
  of a case with `"bucket": "own"` (`-c<n>`) are created by the harness in
  `us-central1`, STANDARD, soft delete off. A `{new_bucket}` (`-n-<n>`, one per
  case that uses it) is created by the case's own request, so its location,
  storage class and soft delete setting are whatever that request says (for
  example NEARLINE with a 7-day soft delete, or the `us` multi-region); the
  case validator only requires `enforced` public access prevention and an
  explicit soft delete duration, and the harness adds the label to the request
  on the wire (and hides it again in the stored response, so the case files and
  recordings do not carry it). Own and `{new_bucket}` buckets are deleted
  right after their case; the shared bucket at the end. The report states how
  many requests the run sent to the live service and how many HTTP 429
  responses it retried. The sanitized reference responses are saved
  (`--record FILE`, default `reference-recording.json` in the output
  directory), with the storage.proto revision and the grpcurl version.
- **Cleanup** never stops at the first failure. For each bucket it reads the
  bucket, clears a retention policy, a default event-based hold and requester
  pays (only what is set), deletes notification configs and managed folders,
  deletes every object version (a 403 is answered by releasing the holds and
  the unlocked retention with `overrideUnlockedRetention=true`, then deleting
  again), deletes the bucket and confirms a 404. A managed folders listing that
  is refused (412 when uniform bucket-level access is off, 403 without
  permission) counts as no folders. All problems are collected and reported at
  the end, together with whether the bucket itself was confirmed deleted. A run ends by listing every bucket with the run's prefix
  and cleaning up whatever is left.
- **Sweep** (`cargo xtask differential sweep`, same environment variables)
  lists the buckets whose names start with `gcsmimic-diff-` and carry the
  `gcs-mimic-differential=1` label, and cleans each one up the same way. A
  bucket with the prefix but without the label is left alone and named in the
  report (a case that replaces a bucket's labels can leave its own bucket like
  this after a crash), so you can delete it by hand. Use it after an
  interrupted run (Ctrl-C, a crash, a killed process); it reports the cleanup
  of labelled buckets by number, not by name. (The final listing of a run uses only the run's
  own random prefix and does not need the label.)
- **Rate limits.** Cloud Storage documents these limits
  (<https://docs.cloud.google.com/storage/quotas>): bucket creation and
  deletion about one request every two seconds per project, one metadata update
  per second per bucket, one write per second to the same object name. In Live
  mode the harness waits 2 seconds before a bucket creation or deletion and
  1 second before any other request that changes state (including cleanup). A
  429 is waited out with a doubling delay (4 s first) and retried up to 5
  times; the retries are counted per step and in the report header. Self-check
  and replay never wait.
- **Replay** (`--reference FILE`) compares gcs-mimic with a saved recording and
  never contacts Google, so a fix can be checked offline. Re-record when a case
  changes; a recording whose steps no longer match the case is reported as
  an error, and so is one made under another `bucket` mode (shared, own,
  none) of the case. The mode is stored per case beside the step fingerprints
  rather than inside them, so a recording made before it was stored still
  replays; its mode is simply not compared. The `expect_status` of a step is not part of its
  fingerprint, and a replay judges the recorded response against the case's
  current `expect_status`, so correcting that value needs no new recording. `--record FILE` in self-check mode records gcs-mimic itself, which
  gives an offline fixture for testing the replay path.

To refresh one case, record only that case in Live mode, then merge the partial
recording into the existing reference offline:

```sh
GCS_MIMIC_DIFF_PROJECT=<project> GCS_MIMIC_DIFF_TOKEN="$(gcloud auth print-access-token)" \
  cargo xtask differential run --filter bkt-notifications --record partial.json
cargo xtask differential merge compatibility/differential/reference/gcs.json \
  partial.json --out merged-reference.json
```

Review `merged-reference.json` before replacing the committed reference. The
merge replaces complete cases by ID, retains all other cases and their order,
and requires a partial with `source: "live"` and nonzero reference requests.
Self-check recordings (`source: "self-check"`) are refused on either side.
For the existing live reference, a missing `source` is accepted only on a base
with nonzero reference requests; an unmarked partial must be re-recorded.
Zero-request bases, empty partials, unknown or duplicate
case IDs, failed cases or steps, dates older than the base, incompatible formats,
and different storage.proto revisions or tool metadata (including grpcurl) are
also refused.
Use the same pinned tools for the partial recording. The format stays unchanged;
`generated` becomes the partial recording's date (the latest refresh),
`recorded_dates` retains every contributing recording date, and replay headers
name the combined dates so the latest refresh does not date every observation.
`requests` sums both recordings' reference requests, including requests for
superseded observations and cleanup. The output passes the same redaction leak
guard as an ordinary recording. `--out` may name the base file for an in-place
update after review.

A new case file has no recording in the reference yet, so a full replay
reports it as an error. Record only the new case and pass `--add-new`, which
appends partial cases the base lacks after the existing cases; without it an
unknown case ID is refused, so a mistyped filter cannot add a case silently.
Every other rule above still applies:

```sh
cargo xtask differential merge compatibility/differential/reference/gcs.json \
  partial.json --add-new --out merged-reference.json
```

Other options: `--cases DIR` (default `compatibility/differential/cases`),
`--filter TEXT` (case ids containing the text), `--out DIR` (default a
temporary directory outside the repository), `--binary PATH` (skip the build),
`--evidence FILE` (also write the Markdown report there), `--fail-on-diff`
(exit nonzero on differences) and `--fail-on-note` (exit nonzero when a step
missed its `expect_status` on gcs-mimic). Harness errors, failed cleanups,
redaction leaks and an unmet `expect_status` on the real service always fail.
A case whose only finding is an unmet `expect_status` on gcs-mimic is reported
as `note`, so the self-check can gate CI without hiding those gaps.

The `differential` job of the Foundation checks replays the committed reference
with both flags and without Google credentials, so a change that drifts from the
recording fails CI. It installs the grpcurl version the reference names, which
moves with a new recording rather than through the dependency refresh.

Every file the harness writes is checked first: it must not contain the
project, the project number, the token, a bucket name of the run, the local
endpoint, an email address or a local path. If it does, the requested file is
left untouched, a fully redacted copy (each local path replaced as a whole) is
written to `<file>.redacted`, and the run fails naming only the location of the
leak (case, step, JSON path), never the value. A later clean write of the same
file removes the stale `<file>.redacted`.
Values are replaced by placeholders before they are compared, recorded or
reported. Placeholders keep the type and shape of the value, because a string
and a number, or a different width, are different JSON: `<generation:str:16>`,
`<generation:num:16>` (`<generation:16>` inside ids, URLs and headers),
`<etag:str>` / `<etag:num>` (`<etag:quoted>`, `<etag:bare>`, `<etag:weak>` for
XML and header entity tags), `<time:f3>` (fractional-second digits; `f0`, `f6`),
`<time:num>`, `<time:str>`, `<time:http>`, `<project-number:str>` /
`<project-number:num>`, `<page-token>`, `<entity-id>`, `<email>`,
`<upload-id>`, `<owner>` (XML `Owner/ID`), `<secret>` (HMAC secret) and
`<bucket>`, `<project>`, `<host>` (`<www-host>` for `www.googleapis.com`) and
captured variables. A gRPC answer arrives as protobuf JSON, which drops trailing
groups of three zero digits from a timestamp, so in a gRPC step a timestamp with
fewer digits than the reference, in steps of three, compares equal; more digits
are still a difference, and unit tests pin the digits gcs-mimic keeps. The
infrastructure headers `date`, `server`, `alt-svc`, `x-guploader-uploadid` and
similar are dropped. `content-length` and
`transfer-encoding` are compared as one synthetic header,
`x-harness-body-framing`: `content-length:<ok>` when the declared length equals
the bytes received (`<mismatch N vs M>` otherwise), or `chunked`. A response
body that is not UTF-8 text is compared exactly as `<binary base64:...>`.

Email addresses with a percent-encoded `@` in a URL become
`<email:percent-encoded>`, so raw `@` versus `%40` remains a reported encoding
difference. Both forms are also detected by the leak guard. The local Storage
service agent's project number `0` uses the same `<project-number>` placeholder
as the discovered project number in live service-agent names.
Double-encoded `%2540` becomes `<email:double-percent-encoded>` and is also
detected by the leak guard.

### Case files

One JSON file per case in `compatibility/differential/cases/`; the `id` equals
the file name, and unknown fields are rejected. Areas can add files
independently.

```json
{
  "id": "object-preconditions",
  "description": "What the case checks.",
  "bucket": true,
  "compare": {"ignore_headers": ["vary"]},
  "steps": [
    {
      "name": "upload",
      "method": "POST",
      "path": "/upload/storage/v1/b/{bucket}/o",
      "query": {"uploadType": "media", "name": "{object}"},
      "headers": {"Content-Type": "text/plain"},
      "body_text": "hello",
      "capture": {"gen": "body:generation"},
      "expect_status": 200,
      "compare": {"ignore_body_paths": ["$.items[*].etag"], "body": "auto"}
    }
  ]
}
```

- `bucket` is `true` (default: `{bucket}` is the shared bucket), `false` (no
  `{bucket}`) or `"own"` (the harness creates a bucket for this case alone,
  named by `{bucket}`, and deletes it right after). A case on the shared bucket
  must not change bucket-level state (metadata, IAM, ACLs, notifications,
  managed folders); validation refuses such a step, so a case gives the same
  result alone (`--filter`) or in a full run. `{missing_bucket}` names a bucket
  that does not exist.
- A bucket a case creates itself must be named `{new_bucket}` (or an invalid
  name the service must reject) and its JSON body must set
  `iamConfiguration.publicAccessPrevention` to `enforced` and an explicit
  `softDeletePolicy.retentionDurationSeconds`. Creating a bucket through the
  XML API (`PUT /<bucket>`) or the gRPC `CreateBucket` is refused.
- `"always": true` on a step runs it even after an earlier step failed the
  case (a failed capture, a transport error). Use it for steps that undo state
  that blocks cleanup: switching versioning off, releasing a hold or an
  unlocked retention. Steps that did not run are recorded as skipped.
- `"local_choice": {"reason": "<reason>", "mimic_status": 200}` marks a step
  gcs-mimic answers differently from the real service on purpose. The report
  lists the reason instead of differences, `expect_status` then speaks only for
  the real service, and an answer other than `mimic_status` is a `note`, so the
  marker cannot outlive the choice. If gcs-mimic's answer changes state a later
  step reads, give the step a case of its own.
- `"pap_reset": "<reason>"` marks a bucket `PUT`, or a `PATCH` that nulls
  `iamConfiguration`, which may reset public access prevention to its default
  (a `PUT` replaces the resource, so omitting the setting can reset it). Without
  the reason the validator refuses such a step unless its body sets the setting
  to `enforced`; `expect_status` is no exemption, because an expected refusal
  can turn out to be an applied write.
- Every `PUT .../iam` with literal bindings, whatever `expect_status` it has (an
  expected refusal can be applied, as an empty `members` list was), must keep
  `roles/storage.admin` or `roles/storage.legacyBucketOwner` for
  a `projectOwner:` or `projectEditor:` member. The policy replaces the whole
  bucket policy; without these bindings the measuring account is locked out and
  every later step answers 403. Build the body from the policy `iam-get` shows
  and add only the change under test.
- `body_base64` sends binary bytes (a gzip stream); it is exclusive with `body`
  and `body_text`.
- `{new_bucket}` is the same name for every step of one case and a different
  name for each case, so cases never reuse a name another case just deleted.
- Variables: `{project}`, `{bucket}`, `{new_bucket}`, `{missing_bucket}`,
  `{object}`, `{object2}` (unique per case) and captured values. `{{` and `}}`
  write literal braces. A variable must be defined before its first use.
- `body` is JSON; `body_text` is raw text. Query values are percent-encoded by
  the harness; write paths already encoded.
- `capture` maps a name to `body:dotted.path` or `header:name` /
  `header:name?param` (a query parameter of the header value, such as the
  upload ID in a resumable `Location`).
- `compare` (case level, merged into each step) accepts `ignore_headers`,
  `only_headers`, `ignore_body_paths` (`$.a.b[*].c`, covering the subtree) and
  `body` (`auto`, `json`, `text`, `ignore`). `"compare": false` on a step runs
  it without comparing it.
- `protocol` defaults to `http` (the JSON API). The other values are described
  below.

#### XML API steps

`"protocol": "xml"` sends the step over the same `curl` transport to the
path-style XML endpoint (`/{bucket}/{object}`, `Authorization: Bearer` on the
real service), so `method`, `path`, `query`, `headers` and `body_text` work as
for JSON steps; set `Content-Type` explicitly on a PUT. A response whose
`content-type` contains `xml` and parses is compared as an ordered tree, not as
text: every element becomes `{"name", "attrs"?, "text" | "children"}`, the root
also carries a canonical `declaration`, so element order, nesting and missing or
extra elements are differences. Paths in `ignore_body_paths` address that tree
(`$.children[2].text`). Leaf text is normalized by tag (`ETag`, `Generation`,
`LastModified` and any timestamp) and for bucket names, and
`x-goog-generation` and `x-goog-copy-source-generation` headers are replaced.
A document that is not well formed is compared as text. Signed URLs are not
covered because they need a service-account key.

#### gRPC steps

`"protocol": "grpc"` replaces `method` and `path` with `rpc`, the full name
such as `google.storage.v2.Storage/GetBucket`. `headers` are gRPC metadata
(for example `x-goog-request-params`, which the real service needs for routing:
the key is the variable name of the RPC's `path_template` in `storage.proto`,
`bucket=projects%2F_%2Fbuckets%2F{bucket}` for most RPCs and
`project=projects%2F{project}` for `ListBuckets`, with the value
URL-encoded like the SDKs do), `body` is the request message as proto3
JSON, and an array of objects sends a client-streaming call. `query`,
`body_text` and `compare.only_headers` have no meaning here.

Write `FieldMask` paths (`readMask`, `updateMask`) in snake_case
(`storage_class`, `checksums.md5_hash`): grpcurl sends the strings unchanged,
and a camelCase path is an unknown path, not the field.

- `grpcurl` is run with the googleapis protos under `src/grpc/proto`, the same
  pinned files (`SHA256SUMS`) the gcs-mimic build uses; the real service does
  not offer reflection. gcs-mimic is started with a plaintext gRPC listener on a
  loopback port; the live side is `storage.googleapis.com:443` over TLS.
- The bearer token reaches `grpcurl` through an environment variable that its
  `-expand-headers` option expands, so it never appears on the command line.
  A case header containing `${` is rejected (it would expand from the
  environment), and `grpcurl` runs with a cleared environment.
- The compared `status` is the gRPC status code (0 is OK, 5 NOT_FOUND, 9
  FAILED_PRECONDITION). A successful body is the response message (an array
  when a server-streaming call returned several); a failed call's body is
  `{"code", "message", "details"?}`; when grpcurl cannot render the status
  (a detail of a type outside the loaded protos) it is `{"code", "raw"}`.
  `expect_status` takes the gRPC code. `nextPageToken` is compared only as
  present or absent. grpcurl silently drops request fields that the pinned proto
  does not define, so a recording names the proto revision and the grpcurl
  version it was taken with.
- `projects/<number>` resource names are normalized like `projectNumber`.
- Without `grpcurl` on `PATH`, every case that has a gRPC step is skipped and
  listed under "Skipped" in the report; the other cases still run and the run
  does not fail.

Three things are never sent, and nothing is sent at all when a check fails.
Validation rejects them in the case file, and the same rules run again on the
expanded request (path, query and headers percent-decoded, body included)
right before it leaves, so a captured variable, an encoded path, a header or
an XML body cannot get around them:

- Locked retention: `lockRetentionPolicy` or `LockBucketRetentionPolicy` (any
  case or underscores), a path, query or header that names both lock and
  retention, a value `locked`, `isLocked` other than false, a retention mode
  that is Locked, a number or a variable, `<Mode>Locked</Mode>`.
- Public access: `allUsers` or `allAuthenticatedUsers` anywhere (including an
  XML `AllUsers` scope), a `public*` `predefinedAcl`,
  `predefinedDefaultObjectAcl` or `x-goog-acl`, and a `publicAccessPrevention`
  other than `enforced` (the invalid probe value `bogus` is allowed).
- HMAC keys: `POST .../hmacKeys` naming a service account, whose response would
  carry a secret. Any `secret` field is replaced by `<secret>` regardless.

Replay refuses a recording taken from a different case definition: each
recorded step carries a fingerprint of its method or RPC, path template,
query, headers and body, and a mismatch (also when the recording holds an
error) is reported as an error to record again.
