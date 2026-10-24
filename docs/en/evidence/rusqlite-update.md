# rusqlite dependency migration

<!-- markdownlint-configure-file {"MD013": false} -->

Verified on 2026-10-24 with Rust 1.99.1, Cargo 1.99.1 and the committed lockfile
on aarch64-apple-darwin. This is a dependency update with no changed feature IDs,
implementation statuses, catalog schema, SQL or application/test source.

Toolchain inspection with `rustc -Vv`, `cargo -V` and `rustup show` confirmed:

| Command | Observed build input |
| --- | --- |
| `rustc -Vv` | 1.99.1; commit `58cf8f9c35b355199727627d07ae34a4e2846d67` dated 2026-10-13; host `aarch64-apple-darwin`; LLVM 23.1.1 |
| `cargo -V` | `cargo 1.99.1 (5f94df478 2026-08-27)` |
| `rustup show` | Active `1.99.1-aarch64-apple-darwin` from the repository's `rust-toolchain.toml`; installed targets `aarch64-apple-darwin` and `x86_64-pc-windows-gnu` |

## Official package provenance

The official [rusqlite index](https://index.crates.io/ru/sq/rusqlite) selected
0.41.0 as the highest numeric, non-yanked stable version. It was published at
2026-10-24T10:35:41Z; the [publisher release](https://github.com/rusqlite/rusqlite/releases/tag/v0.41.0)
followed at 10:36:00Z. The tagged `Changelog.md` directs modern releases to that
release page. Publisher repository metadata reports `archived: false` and a
last push at 10:36:00Z on the same date.

Downloaded official archives were hashed and matched against their index
checksums before adoption. Compressed package sizes are not binary, RSS or
container growth measurements.

| Package | Previous | Selected | License | Compressed bytes | Declared Rust | SHA-256 |
| --- | --- | --- | --- | --- | --- | --- |
| rusqlite | 0.40.2 | 0.41.0 | MIT | 181395 | 1.89.0 | `9c026b4444579e8d754556c6c75d040d69c4a44283cdb648ab8a8d5965c2cfa8` |
| libsqlite3-sys | 0.38.2 | 0.39.0 | MIT | 2759088 | 1.88.0 | `b0e7f1cd558a82cc3af091ed13f0f03846f89851c60a10713ce5cfd484195494` |

The binding is non-yanked and was published at 2026-10-24T10:35:02Z, checked in
its [official index](https://index.crates.io/li/bs/libsqlite3-sys). Sources are the
[rusqlite archive](https://static.crates.io/crates/rusqlite/rusqlite-0.41.0.crate)
and [binding archive](https://static.crates.io/crates/libsqlite3-sys/libsqlite3-sys-0.39.0.crate).
The rusqlite release's historical “Lower MSRV to 1.88.0” entry differs from the
actual release: both index metadata and distributed `Cargo.toml` and
`Cargo.toml.orig` specify **1.89.0**. The binding's distributed manifests specify
1.88.0. Both use edition 2024 and fit the unchanged tested Rust 1.99.1 pin.

## Compatibility assessment

`cargo update -p rusqlite --precise 0.41.0` changed only rusqlite and its required
`libsqlite3-sys ^0.39.0` resolution. Other locked packages and checksums did not
change. `default-features = false` and `features = ["bundled"]` remain unchanged.
`cargo tree --locked --offline -e features -i rusqlite` reports only `bundled`
and its implied `modern_sqlite`; no cache, pooling, SQLCipher, bindgen, WASM or
Rust extension features were added. The binding still uses prebuilt bundled
bindings and the existing C compiler build dependency.

The SQLite amalgamation advances from **3.53.2 to 3.54.0**. Its header's source ID
is `2026-10-09 15:46:58 be8d059e9a49089ab2dce5ed26dd87aaf598fdc5fbe0b107c0dd758464bcd5e3`,
matching the [official SQLite release](https://sqlite.org/releaselog/3_54_0.html).
SQLite source retains its public-domain notice; the Rust wrappers are MIT.
The [file-format policy](https://sqlite.org/formatchng.html) supports newer
SQLite reading older databases; the direct upgrade check below exercises this
application's schema and external blobs as well.

The publisher changes statement column inspection to iterators, blob zero-fill
lengths to `u64`, and callback ownership/bounds. The application uses none of
those changed APIs. PRAGMA helpers now check names through `pragma_pragma_list`;
all current helper names are valid and are exercised by catalog tests. The
minimum system SQLite rises to 3.45.3, while this application compiles the
bundled source. Existing `busy_timeout` calls retain the five-second bound.

The binding stops enabling `SQLITE_ENABLE_MEMORY_MANAGEMENT`. The application
does not call `sqlite3_release_memory`; its [optional cache-release behavior](https://sqlite.org/compile.html#enable_memory_management)
is separate from the retained connection cache limit. Source comparison keeps
`SQLITE_THREADSAFE=1` and API armor. Existing bundled C extensions remain as
upstream configured; disabling Rust extension features does not remove those
C compile options. No durability, synchronization or authorization setting was
relaxed. Existing tests assert WAL, synchronous FULL, foreign keys, the 2 MiB
cache, disabled mmap, busy timeout and schema 14. Memory/performance effects
have not been benchmarked, so this change makes no resource improvement claim.

SQLite 3.54.0 also changes query planning, temporary-file handling and Windows
mutex implementation, and rejects column add/drop/rename operations on special ROWID
names. Existing catalog migrations do not alter those names or use the new
optional SQL functions. Targeted migration/listing tests cover the actual SQL;
Windows and Linux runtime behavior still need their normal platform lanes.

## Executed verification

Commands use the rustup Cargo at the front of PATH and the pinned toolchain.

| Command/check | Result |
| --- | --- |
| Before update: `cargo test --locked --offline --test catalog --test object_storage --test resumable_storage` | PASS: 21 catalog, 23 object storage and 4 resumable storage tests |
| `cargo update -p rusqlite --precise 0.41.0`; `cargo fetch --locked` | PASS: only the two intended package versions changed |
| `cargo test --locked --offline --lib storage:: --test catalog --test object_storage --test resumable_storage` | PASS: 61 selected storage unit tests, including migrations, rollback, bounded admission and explicit SQLite settings; integration targets were filtered, then run without that filter below |
| After update: `cargo test --locked --offline --test catalog --test object_storage --test resumable_storage` | PASS: the same 48 integration tests |
| `cargo test --locked --offline --test bucket_http --test basic_contract_http --test object_http --test object_metadata_http --test object_versioning_http --test auth_http` | PASS: 215 HTTP regression tests (60 bucket, 12 basic contract, 27 object, 5 metadata, 9 versioning, 102 authorization) |
| `cargo fmt --all --check` | PASS |
| `cargo clippy --workspace --all-targets --locked --offline -- -D warnings` | PASS |
| Final `PATH="$HOME/.cargo/bin:$PATH" just check` after rebase onto `8138d13` | PASS, exit 0: 778 passed tests across 38 result groups, 0 failed/ignored; formatting, workspace clippy/tests, compatibility, hermetic publication and shell checks passed |
| Final `PATH="$HOME/.cargo/bin:$PATH" just check-dependencies` | Exit 1: rusqlite and all other direct Rust/toolchain/SDK/Action inputs CURRENT; only the existing distroless digest update remains |
| `cargo metadata --locked --offline --format-version 1` | PASS: rusqlite 0.41.0 and libsqlite3-sys 0.39.0 resolved with their declared Rust 1.89.0/1.88.0 requirements |

The integration coverage includes rejection without catalog/sidecar mutation,
committed-WAL recovery after process exit, migration rollback, generation
non-reuse, all four preconditions, concurrent create/overwrite/delete races,
blob reclamation recovery and persisted resumable checkpoints. No duplicate
test of unchanged implementation was added.

A separate local upgrade check saved the old application binary before the
update. With rusqlite 0.40.2 / SQLite 3.53.2 it created a versioned bucket with
a label and a create-if-absent object, saved complete bucket/object JSON and
shut down cleanly. The updated binary opened that same data directory: readiness
passed, complete JSON matched byte-for-byte, and downloaded bytes matched.
A repeated create-if-absent returned 412 `conditionNotMet` and preserved the
record. A matching-generation overwrite succeeded with a larger generation;
listing contained both generations and the original generation still returned
the old bytes. This fixture remained outside the repository.

The [raw freshness output](rusqlite-dependencies.txt) records the remaining
distroless/cc-debian13:nonroot digest drift from
`83b9b50586253821cf63ffcec87d3d94265a093ad30da232039af962dd8f4edf` to
`3ee00291cf386ec52a552e6a858d555debd74776eb4bfa53fa3df2cd6677515e`.
Distribution owns that separate release gate and review by 2026-10-25; no
exception hides it.

Rebase onto the integrated initializer baseline preserves `serde_json raw_value`,
the approved libc fsutil/xattr and Unix `O_NONBLOCK` uses, and both CLI and
initializer proptest scopes. Only the rusqlite dependency versions change;
the initializer's earlier freshness-check result remains historical evidence.

An independent Astra review of the dependency migration found no critical,
major or required-fix findings.

The final `just check` ran once in the coordinator's exclusive execution window
after rebase. Only documentation and dependency-status evidence changed after
that passing run; the validated runtime source, manifest and lockfile are intact.

## Remaining validation

Container builds, official SDK runs, native Linux/Windows execution,
benchmarks, power-loss testing, real-cloud differential tests and remote CI
were not run for this migration. Existing recovery limitations remain unchanged.
