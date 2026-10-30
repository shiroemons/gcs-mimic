# Development and verification

<!-- markdownlint-configure-file {"MD013": false} -->

[日本語](../ja/development.md)

Requires rustup and curl for the explicit online freshness check. Building and `just check` need no Python; `just check-sdk` runs the Python SDK probe on the uv-managed Python 3.14.10. Rust 1.99.1 is the verified latest stable compiler; edition 2024 is explicit. No Google credentials or project are needed.

## Development commands and push checks

Install [just](https://github.com/casey/just#installation) 1.56.0 or newer to use the checked-in
`Justfile`; `just` lists the available recipes. The recipes are intended for
macOS and Linux with Bash. The full pre-push check also requires Go, jq, curl,
the pinned Ruby/Bundler/gems from [the Ruby harness](../../sdk-tests/ruby/README.md),
Node.js 24+ with npm, uv, openssl, Docker Engine/Desktop, Compose and Buildx.
PHP and Composer for the SDK scorecard run in Docker. Docker must be running and able to
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
2. The complete Go/Ruby SDK suites, then the SDK scorecard gate, which requires
   every Go, Ruby, Node.js, PHP and Python case to pass against gcs-mimic.
3. AMD64 and ARM64 container checks **in parallel**. Each builds its own image,
   then immediately runs its smoke tests without waiting for the other build.

A failed stage prevents the next stage from starting. The container stage waits
for both workers and their cleanup, then fails if either worker failed. Output
lines carry `[linux/amd64]` or `[linux/arm64]`, including build errors. The existing
harness isolates image tags, Compose projects, volumes and dynamically assigned
host ports for each worker.

Run `just check` while editing and reserve the SDK/container checks for the final
source. Parallel builds use more CPU and memory; on a constrained Docker host,
use `just --jobs 1 pre-push` or `just --jobs 1 check-containers` to run them in
sequence. To investigate one architecture, use `just check-container linux/amd64`
or `just check-container linux/arm64`. The direct
`bash scripts/smoke-containers.sh` command still runs its requested platforms in
sequence. Dependency downloads and container pulls can require network access;
this is not an entirely offline command.

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
`scripts/with-test-tmpdir.sh`. On macOS, that wrapper attaches a temporary RAM
disk (`GCSM_TEST_RAMDISK_MB`, default 4096), uses it as `TMPDIR`, raises
the default pool to 4, and detaches the disk on exit. Tests keep their real file
synchronization. macOS implements it with `F_FULLFSYNC`, which flushes the
system disk and otherwise dominates test time. Concurrent tests on the system
disk delay processes enough to break wall-clock deadline tests, so the pool
stays serial there. Set `GCSM_TEST_RAMDISK=0` to use the normal `TMPDIR`.
If the RAM disk cannot be created, the wrapper warns and continues serially. On
other platforms the wrapper changes nothing, and CI runs the binaries serially
on its disk. Local macOS checks and CI therefore run tests under different
timing, so a timing-sensitive failure can appear in only one of them; treat it
as a test defect in either place. These settings use the `GCSM_TEST_` prefix
because the server reads every `GCS_MIMIC_` and `FAKE_GCS_` variable as its own
setting and rejects unknown names.

`bash scripts/sdk-go.sh` remains Go-only for the existing CI job; the SDK recipe
explicitly passes `--with-ruby`. It requires installed pinned Ruby gems and fails
if they are absent. It runs the Ruby origin guard, permissive/strict basic methods
and strict signed GET/PUT. `GCS_MIMIC_SDK_SCHEME=https just check-sdk` verifies
both languages with the exported local CA.

The recipe then runs `bash scripts/sdk-scorecard-check.sh`. It runs the
[SDK scorecard](../../sdk-scorecard/README.md) probes for the official Go, Ruby,
Node.js, PHP and Python SDKs against a release build of the checked-out source,
writing results to a temporary directory instead of `sdk-scorecard/results`, and
fails with the SDK and case ID of every case that does not pass. The scorecard
itself only scores, so this gate is what makes a Node.js, PHP or Python
regression fail `just check-sdk`. It reruns the Go and Ruby probes because a
scored run requires all five SDKs, and the release build takes noticeably longer
than the debug build. Failed results are kept and their location is printed. Signature endpoint/payload caveats are
recorded in [the signed-access evidence](evidence/signed-urls.md).

No Git hook is installed and no commit or push is performed. Re-run the checks
after changes. A local pass does not establish native amd64 or GitHub Actions
success: the configured remote lanes must also pass after pushing.

The Foundation checks workflow restores Cargo dependency builds with
`Swatinem/rust-cache`, and only runs on `main` save them, so branches and pull
requests reuse the latest `main` cache. A newer push cancels the previous run
for the same branch or pull request; `main` keeps every run. CI builds debug
profiles with `CARGO_PROFILE_DEV_DEBUG=line-tables-only` to shorten linking,
and Windows tests use `RUNNER_TEMP` as their temporary directory. The release
workflow's native test runs use the same two settings but no Cargo cache, so
release archives always build from a clean target directory.

Changes that touch only Markdown files or `docs/` skip Foundation checks and
run the Documentation checks workflow instead. It runs `scripts/check-docs.sh`,
the same documentation checks that `scripts/check.sh` runs: the feature ledger
and generated matrices, release-note links and the Docker Hub overview. Add any
new check that reads committed documentation to that script so both workflows
run it. A change that also touches other files runs Foundation checks as usual,
and release workflows always run the full checks. Use manual dispatch to run
Foundation checks for a documentation-only commit.

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
It also type-checks the Windows MSVC, Linux musl and macOS ARM64 release targets
other than the host through `scripts/check-cross.sh`, installing missing Rust
targets with `rustup`. C dependencies compile to empty stub objects there, so
this catches `cfg` and platform-API mistakes but not link or runtime failures.
Foundation CI's `portability` job runs the tests natively on one target per
OS family on every push; the release matrix still runs only for release tags
and manual dispatch. Release workflows skip `portability` because their build
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

`src/fsutil.rs` does not exist yet. The `shared-publication-io` slice creates it,
moves the relevant platform calls there and removes their former exceptions.
That slice must add narrow exceptions next to each unsafe operation, document
pointer/handle ownership and platform preconditions, and verify all affected
platforms. Do not add a placeholder module or an exception before it is needed.

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
It is a zero-tolerance gate over server production code: any finding fails,
and it prints every occurrence. There is no baseline. The only exceptions are:

- a reasoned `#[expect(<lint>, reason = "...")]` for a Clippy or rustc lint
  (see [below](#expect-for-restriction-lints));
- a reasoned `// xtask-lint: allow(text::<rule>) -- <reason>` comment for a
  text rule (see [Reasoned suppressions](#reasoned-suppressions));
- exactly one definition of `fn unavailable`, the shared storage error
  mapping, in `src/storage/failure.rs`. A definition anywhere else fails, and
  so does a second one in that file or a missing one.

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
| `clippy::disallowed_methods` | The methods listed in `clippy.toml`: unsupervised spawns (`tokio::spawn`, `tokio::task::spawn`, `tokio::task::spawn_local`, `Handle::spawn`, `Runtime::spawn`, `std::thread::spawn`, `std::thread::Builder::spawn`) outside `src/task.rs`, and rusqlite's `Connection::execute`, `query_row`, `query_one` and `prepare`, which prepare their SQL on every call. Calls through a `Transaction` resolve to the `Connection` methods; `Statement` methods from `prepare_cached`, `execute_batch`, `spawn_blocking`, `JoinSet::spawn` and scoped threads are not listed. |
| `text::path_attribute` | `#[path]` module attributes. |
| `text::string_error_result` | `fn` signatures and type aliases of `Result<_, String>`. |
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

Latest verified direct releases: Tokio 1.54.0, Hyper 1.12.0, hyper-util 0.1.21, http-body-util 0.1.5, serde_json 1.0.154 and rusqlite 0.40.2. Versions, licenses, sizes and sources are in the [dependency snapshot](../../compatibility/dependency-status.json). Cargo.lock records the full graph. Docker inputs, including the test-only peer curl image, use actual multiarch digests. The snapshot checks both Dockerfile and smoke-script pins.

The object media slice additionally uses bytes 1.12.1, md-5 0.11.0, crc32c 0.6.8 and
getrandom 0.4.3, with the selection reasons and package sizes in the
[dependency policy](dependency-policy.md#dependency-status-evidence).

The Go harness requires Go and jq; it pins Go 1.27.3, storage v1.70.0 and API v0.308.0 with committed go.mod/go.sum. Its high-level HTTP JSON bucket and object listing/Update/Attrs/NewReader/Delete workflows run against an isolated native server; the getting-started guide verifies the same suite against Compose. Raw media upload uses the harness HTTP client; SDK Writer with `ChunkSize=0` exercises multipart metadata, hashes and conditioned publication. Resumable Writer remains rejected and tested. Media/multipart upload tokens require generation-match; GET/DELETE/metadata-update tokens retain their existing acceptance without deduplication (SPEC-013/SPEC-015). `scripts/sdk-go.sh` also starts a second isolated native server with `--auth-mode strict --identity-file config/identities.json`, checks that its ready event reports strict mode, and runs the `TestStrictAuth*` suite against it through `GCS_MIMIC_STRICT_ENDPOINT`. Those clients use an explicit endpoint and an `oauth2.StaticTokenSource` with the public fixture tokens, so no Application Default Credentials or metadata-server lookup occurs; the suite skips only when that variable is unset. Rust `auth_http` and `url_cli` tests cover strict authentication, role enforcement, startup validation and the URL helper offline. See the [strict authentication evidence](evidence/strict-auth.md). Tests make no Google/metadata-server requests. Rust integration tests use isolated temporary catalog directories, including startup failure, generations, condition races, restart and selected SIGKILL publication boundaries. On unix, unit tests also take power-loss snapshots of single-request uploads, resumable finalization and compose/copy/rewrite publication that keep only the file lengths and directory entries the seamed sync points made durable (model and limits in `src/storage/catalog/fault.rs`), so dropping the blob or shard fsync fails them. Renames, unlinks and new-file entries outside the seam, such as recovery's restore-to-staging renames, are not modeled. Every other fsync/deletion instruction window, real power loss and memory/CPU/latency measurements remain unverified. See the [multipart integration evidence](evidence/multipart.md) and [guide transcript](evidence/multipart-guide.txt).

`dependencies check` contacts official Rust, Cargo, GitHub, Debian, Docker Hub and
public GCR metadata. Other Rust tests are offline and use only loopback. All direct
dependencies are compared against non-yanked stable releases, including new majors;
Docker tags/digests and Debian stable codename are checked separately. Exit 1 means
updates; exit 2 means verification failed. Run at slice start and before release
and review exceptions weekly. No manifests are modified and nothing is published
automatically.

Pinned/current-stable Rust CI and native amd64/arm64 container CI are configured. Daily freshness runs at 03:17 UTC every calendar day without cooldown after reaching the default branch. Remote CI/schedule execution remains unverified. Latest-dependency candidate builds and upgrade submission are not implemented. Local arm64 runs natively; local amd64 uses emulation.

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
