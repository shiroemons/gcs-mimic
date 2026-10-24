# Development and verification

<!-- markdownlint-configure-file {"MD013": false} -->

[日本語](../ja/development.md)

Requires rustup and curl for the explicit online freshness check. Python is not required. Rust 1.99.1 is the verified latest stable compiler; edition 2024 is explicit. No Google credentials or project are needed.

## Development commands and push checks

Install [just](https://github.com/casey/just#installation) 1.56.0 or newer to use the checked-in
`Justfile`; `just` lists the available recipes. The recipes are intended for
macOS and Linux with Bash. The full pre-push check also requires Go, jq, curl,
the pinned Ruby/Bundler/gems from [the Ruby harness](../../sdk-tests/ruby/README.md),
Docker Engine/Desktop, Compose and Buildx. Docker must be running and able to
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
2. The complete Go/Ruby SDK suites.
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

`bash scripts/sdk-go.sh` remains Go-only for the existing CI job; the SDK recipe
explicitly passes `--with-ruby`. It requires installed pinned Ruby gems and fails
if they are absent. It runs the Ruby origin guard, permissive/strict basic methods
and strict signed GET/PUT. `GCS_MIMIC_SDK_SCHEME=https just check-sdk` verifies
both languages with the exported local CA. Signature endpoint/payload caveats are
recorded in [the signed-access evidence](evidence/signed-urls.md).

No Git hook is installed and no commit or push is performed. Re-run the checks
after changes. A local pass does not establish native amd64 or GitHub Actions
success: the configured remote lanes must also pass after pushing.

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

Keep `#[allow(unsafe_code)]` on each necessary unsafe statement or its binding,
with an adjacent `// SAFETY:` comment explaining why the operation is safe.
Clippy rejects allowed unsafe blocks without these comments. Callbacks that
convert raw pointer arguments to references must be `unsafe extern "C" fn` with
a `# Safety` contract. Their item-level allow is needed for the unsafe declaration
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

## Direct verification commands

Native archive preparation and the eight-target release CI are documented in the
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
bash scripts/smoke-containers.sh
bash scripts/sdk-go.sh
```

The xtask alias uses locked offline resolution. Fetch all locked packages first. Explicit `+1.99.1` overrides a conflicting `RUSTUP_TOOLCHAIN` environment variable.

Latest verified direct releases: Tokio 1.54.0, Hyper 1.12.0, hyper-util 0.1.21, http-body-util 0.1.5, serde_json 1.0.154 and rusqlite 0.40.2. Versions, licenses, sizes and sources are in the [dependency snapshot](../../compatibility/dependency-status.json). Cargo.lock records the full graph. Docker inputs, including the test-only peer curl image, use actual multiarch digests. The snapshot checks both Dockerfile and smoke-script pins.

The object media slice additionally uses bytes 1.12.1, md-5 0.11.0, crc32c 0.6.8 and
getrandom 0.4.3, with the selection reasons and package sizes in the
[dependency policy](dependency-policy.md#dependency-status-evidence).

The Go harness requires Go and jq; it pins Go 1.27.3, storage v1.70.0 and API v0.308.0 with committed go.mod/go.sum. Its high-level HTTP JSON bucket and object listing/Update/Attrs/NewReader/Delete workflows run against an isolated native server; the getting-started guide verifies the same suite against Compose. Raw media upload uses the harness HTTP client; SDK Writer with `ChunkSize=0` exercises multipart metadata, hashes and conditioned publication. Resumable Writer remains rejected and tested. Media/multipart upload tokens require generation-match; GET/DELETE/metadata-update tokens retain their existing acceptance without deduplication (SPEC-013/SPEC-015). `scripts/sdk-go.sh` also starts a second isolated native server with `--auth-mode strict --identity-file config/identities.json`, checks that its ready event reports strict mode, and runs the `TestStrictAuth*` suite against it through `GCS_MIMIC_STRICT_ENDPOINT`. Those clients use an explicit endpoint and an `oauth2.StaticTokenSource` with the public fixture tokens, so no Application Default Credentials or metadata-server lookup occurs; the suite skips only when that variable is unset. Rust `auth_http` and `url_cli` tests cover strict authentication, role enforcement, startup validation and the URL helper offline. See the [strict authentication evidence](evidence/strict-auth.md). Tests make no Google/metadata-server requests. Rust integration tests use isolated temporary catalog directories, including startup failure, generations, condition races, restart and selected SIGKILL publication boundaries. Every fsync/deletion instruction window, power loss and memory/CPU/latency measurements remain unverified. See the [multipart integration evidence](evidence/multipart.md) and [guide transcript](evidence/multipart-guide.txt).

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
