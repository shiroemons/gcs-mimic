# Repository lint verification

<!-- markdownlint-configure-file {"MD013": false} -->

Verified on 2026-10-24 with Rust 1.99.1
(`58cf8f9c35b355199727627d07ae34a4e2846d67`), edition 2024,
on `aarch64-apple-darwin`. The original lint implementation is commit `4bca9fd`;
this evidence also covers the safety-comment policy and narrower exceptions.
The [contract](../slices/repo-lints.md) has no feature IDs; the compatibility
ledger and generated views are unchanged. Runtime behavior and dependencies
are unchanged.
The line-length rule is disabled for the long table cells and literal output
blocks, preserving captured diagnostics and digests.

## Native checks and policy probes

| Command or check | Observed result |
| --- | --- |
| `just check` | PASS, exit 0: 665 tests passed, zero failed or ignored across 36 test/doc-test suites; fmt, workspace/all-target Clippy, compatibility views, shell and offline publication checks passed. |
| `cargo +1.99.1 fmt --all --check` | PASS on the final source. |
| `cargo +1.99.1 clippy --workspace --all-targets --locked --offline -- -D warnings` | PASS on the final source. |
| `npx --yes markdownlint-cli2 docs/en/development.md docs/en/slices/repo-lints.md docs/en/evidence/repo-lints.md` | PASS with markdownlint-cli2 0.24.0: zero issues in all three files. |
| `CARGO_TARGET_DIR="$(mktemp -d "${TMPDIR:?}/gcs-mimic-lints.XXXXXX")" cargo +1.99.1 clippy --workspace --all-targets --locked --offline -v -- -D warnings` compiler invocations | The newly created, empty target directory forces compilation: all 38 workspace target invocations included both `--deny=unsafe_code` and `--deny=clippy::undocumented_unsafe_blocks`, covering the application library, binary, integration tests (including `catalog`) and `xtask` library/binary/tests. |
| `rustc +1.99.1 --edition=2024 --crate-type lib --deny unsafe_code --emit metadata` with stdin probes | Unsafe without an exception failed with `usage of an unsafe block`; the same operation with a statement-local allow compiled. |
| `rustup run 1.99.1 clippy-driver --edition=2024 --crate-type lib --deny unsafe_code --deny clippy::undocumented_unsafe_blocks --emit metadata` with stdin probes | An allowed unsafe read without a safety comment failed with `unsafe block missing a safety comment`; adding an adjacent `// SAFETY:` comment compiled. |
| `rustup run 1.99.1 clippy-driver --edition=2024 --crate-type lib --emit metadata` with a parser-module stdin probe | `#![deny(clippy::unwrap_used, clippy::expect_used)]` rejected both calls, with `used unwrap() on an Option value` and `used expect() on an Option value` diagnostics. |
| Extracted `count_progress` with `clippy-driver --edition=2024 --deny unsafe_code --deny clippy::undocumented_unsafe_blocks --deny warnings` | The callback compiled without an inner allow; a safe caller failed with E0133; removing the inner unsafe block failed under `unsafe_op_in_unsafe_fn`; removing its safety comment failed with `unsafe block missing a safety comment`. A documented extra unsafe block and nested unsafe impl compiled under the item-level allow; removing that allow rejected the declaration, blocks and impl. |
| `git diff --check` and local-path review | PASS for the slice changes; the existing planning document contains the path-detection regex itself, not an environment-specific path. |

The existing SQLite progress/limit and Unix pre-exec tests passed without
changing their assertions, skipping cases or broad module exceptions.
The parser lint probe does not apply those restrictions to legacy modules.

The two SQLite query-budget tests share one private `unsafe extern "C" fn`
callback with a `# Safety` pointer contract. Its item-level unsafe allowance is
needed for the declaration but extends to its body and nested items. The
dereference remains in an explicit inner unsafe block with a `// SAFETY:` comment;
an inner allow would be redundant and has been removed. The explicit-block and
safety-comment lints still apply, but necessity and safety throughout the
item-level allowance require review. Registration, explicit unregistration,
instruction budgets and final counter assertions are unchanged.
A guard type was not introduced: it would add connection/counter lifetime and
cleanup machinery beyond the shared callback needed for this lint-only change.
Both existing query-budget tests and the page-token test also passed the targeted
`cargo +1.99.1 test --locked --offline --lib object_list_` run (3 passed).

The pointer contract requires a live, aligned counter whenever SQLite can invoke
the callback, rather than until explicit unregistration. On a test panic,
unwinding skips that unregistration and ends the counter's lifetime before the
connection is dropped. This is safe for these two tests: any active query's
statements unwind while the caller's counter is still live; subsequent cleanup
only finalizes cached statements and closes the in-memory connection, without
preparing or stepping SQL. Source inspection of locked rusqlite 0.40.2
(`Connection::drop`, `InnerConnection::close`) and libsqlite3-sys 0.38.2's bundled
SQLite 3.53.2 (`sqlite3Close`, `sqlite3LeaveMutexAndCloseZombie`) confirms that this
cleanup does not invoke the progress callback for these plain-table fixtures.
There are no custom virtual tables or SQL-running destructors in these tests.
This is a source-based safety argument, not a panic-injection test or a guarantee
for arbitrary connection cleanup; changes to the fixtures, cleanup or dependency
versions must revisit it. SQLite documents progress callbacks during
[prepare/step operations](https://www.sqlite.org/c3ref/progress_handler.html).

The required review checklist for later configuration, initializer and import
slices now checks each parser module's unwrap/expect deny and confines test
exceptions to test code. This is an explicit review gate, not an automatic
opt-in detector. Parser restructuring is deferred until actual shared parsing
modules justify a parent module.

## Windows coverage and limits

Installed the pinned Windows standard library with:

```sh
rustup target add x86_64-pc-windows-gnu --toolchain 1.99.1
cargo +1.99.1 clippy --workspace --all-targets --target x86_64-pc-windows-gnu --locked --offline -- -D warnings
```

The full cross-target command exited 101 before application checking because
the host has no MinGW C compiler. The observed build diagnostic was:

```text
error occurred in cc-rs: failed to find tool "x86_64-w64-mingw32-gcc": No such file or directory (os error 2)
```

As a narrower check, the already-produced Windows `windows-sys` metadata was
used to compile exact source extracts on the Windows target. One stdin probe
contains `publish_blob` in a minimal module with a public caller. The other
contains all of `server_command` and the beginning of `stop_server` through its
console signaling block; shared timeout and JSON-event handling are excluded.
Minimal imports and a placeholder binary-name environment value complete the
probe. A probe-only unused-variable allowance accommodates the truncated
function's unused output parameter. Both probes were rerun with the safety-comment
lint and passed `clippy-driver` with these flags:

```text
--edition=2024 --target x86_64-pc-windows-gnu --crate-type lib
--deny unsafe_code --deny clippy::undocumented_unsafe_blocks
--deny warnings --emit metadata
--extern windows_sys=<windows-sys-metadata>
-L dependency=target/x86_64-pc-windows-gnu/debug/deps
```

The exact source extraction commands used for those stdin probes were:

```sh
(
  printf '%s\n' 'mod publication {' 'use std::{fs, io, path::Path};'
  sed -n '168,216p' src/storage/objects.rs
  printf '%s\n' '}' \
    'pub fn publish(source: &std::path::Path, destination: &std::path::Path) -> std::io::Result<()> { publication::publish_blob(source, destination) }'
)

(
  printf '%s\n' '#![allow(unused_variables)]' \
    'use std::{io::BufRead, process::{Child, Command}};'
  sed -n '78,124p' tests/common/mod.rs
  printf '%s\n' '}'
)
```

Pipe each group to the `clippy-driver` command above, adding stdin `-` and
`--out-dir "$TMPDIR"`. Substitute the matching
`target/x86_64-pc-windows-gnu/debug/deps/libwindows_sys-*.rmeta` for
`<windows-sys-metadata>` and set `CARGO_BIN_EXE_gcs-mimic=gcs-mimic.exe` for the
console probe. The first range is the whole `publish_blob` function; the second
is `server_command` plus `stop_server` through the closing Windows signaling
block. These ranges correspond to the source verified in this evidence.

This checks all current Windows unsafe sites, including `MoveFileExW`,
`GetConsoleCP`, `AllocConsole` and `GenerateConsoleCtrlEvent`, against the Windows
types and effective deny policy. It does not establish a full Windows build or
Windows runtime success. The full Windows cross-target command was retried after
the safety-comment changes and again exited 101 in the bundled SQLite C build
because MinGW is unavailable. The MSVC target and remote Windows CI were not
run; neither were other native release targets.

The existing [binary workflow](../../../.github/workflows/binaries.yaml) supports
`workflow_dispatch` and has native MSVC lanes on `windows-2025`
(`x86_64-pc-windows-msvc`) and `windows-11-arm`
(`aarch64-pc-windows-msvc`). They run workspace/all-target Clippy with
`-D warnings` and workspace tests, including console initialization and
CTRL-BREAK shutdown. The `build` job needs `validate`, whose final step is
`cargo xtask dependencies check`. Because the freshness check currently reports
the distroless digest update below, a dispatch is expected to stop at `validate`
before reaching Windows; this is an inference, not an observed workflow run.
The workflow was not dispatched because it affects external systems. Once the
freshness gate is resolved, these native MSVC Clippy and test lanes are the first
pending verification to perform.

## Dependency freshness

`just check-dependencies` completed with exit 1 because a container input has
an update. Rust 1.99.1, all direct Rust dependencies and the checked Go/Ruby
inputs were current. The detected update was:

```text
docker distroless/cc-debian13:nonroot: UPDATE_AVAILABLE current=sha256:83b9b50586253821cf63ffcec87d3d94265a093ad30da232039af962dd8f4edf latest=sha256:3ee00291cf386ec52a552e6a858d555debd74776eb4bfa53fa3df2cd6677515e
```

That container update is outside the lint slice and was reported to the
integration owner. No dependency or container pin was changed.

SDK, container builds, full guide journeys and performance benchmarks were not
run for this compile-time policy change. No performance claim is made.
