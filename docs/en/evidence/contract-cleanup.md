# Verification: breaking changes before the 1.0.0 contract freeze

This records the checks run for [DESIGN-103](../slices/contract-cleanup.md) on the integrated `0.20.0` source, on macOS arm64 with Rust from `rust-toolchain.toml` and Go 1.27.4. Paths are relative to the repository root.

## Final source

| Check | Result |
| --- | --- |
| `cargo test --locked --workspace --no-fail-fast` | 3427 passed, 0 failed, 1 ignored |
| `cargo xtask compatibility check` | 609 feature rows validated; both feature matrices current |
| `cargo xtask lint` | 0 new production violations, 0 ratcheted findings |
| `cargo clippy --locked --all-targets` | No warnings |
| `bash scripts/check-docs.sh` | Passed, including the environment namespace, ledger, help and option-list checks |
| `bash scripts/test-release-recipes.sh` | Passed: recipes and the Go package must match the manifest version |
| `cd go && go test -race -count=1 ./fakestorage` | Passed |
| `cd go && GCSM_BINARY=<checkout>/target/debug/gcs-mimic go test -race -count=1 -tags integration ./fakestorage` | Passed against the binary built from the same source |

## Per change

Each change was checked before it was committed; these are the suites that cover it.

| Change | Command | Result |
| --- | --- | --- |
| Contract guard tests | `cargo test --locked --test it_admin admin_contract`; `--test it_process` ready field, help digest and PID 1 cases; `--lib diagnostics::` | All passed |
| `buckets_mode` | `cargo test --locked --lib`; `--test it_process`, `--test it_import_seed`, `--test it_admin` | 2072 library tests; 108, 94 and 79 (1 ignored) integration tests passed |
| `connection_timeout_ms` removal | `cargo test --locked --lib config::`; `--test it_protocol phase_deadlines`; `--test it_process foundation` | 93, 10 and 54 passed |
| Underscore spellings | `cargo test --locked --lib config`; `--test it_process an_underscore_option` | 144 and 1 passed |
| `backend=filesystem` | `cargo test --locked --lib config`; `--test it_process -- option_aliases fake_gcs_server_command` | 145 and 11 passed |
| Synchronization version check | `cargo test --locked --lib server::admin::sync` and `sync::handshake`; `--bin gcs-mimic sync_cli`; `--test it_sync`; `--test it_admin management_routes_refuse` | 29, 1, 13, 33 and 1 passed |
| `sync approve --revision` | `cargo test --locked --bin gcs-mimic sync_cli`; `--test it_sync sync_cli` | 13 and 13 passed |
| Go version check | `go test -race -count=1 ./fakestorage`, with and without `-tags integration` | Passed |
| `compat_profile=fake-gcs-server` | `cargo test --locked --workspace --no-fail-fast` | 3427 passed, 0 failed |

## Release gates on the integrated source

`just pre-push` passed on commit `12c73149` in 17m25s:

| Stage | Result |
| --- | --- |
| `just check` | 3423 Rust tests passed in 16 test binaries, 0 failed, 1 ignored; Clippy and the production lint gate passed on the host and on `x86_64-pc-windows-msvc`, `x86_64-unknown-linux-gnu` and `x86_64-unknown-linux-musl`; documentation and script checks passed |
| `just check-sdk` | The pinned Go, Ruby, Node.js, Python, PHP, Java, .NET (net10.0 and net11.0) and Rust suites passed in the native profile. Four cases were skipped by their own conditions: Go V2 signing and two Node.js cases need HTTPS, and one Node.js case runs in the strict profile only |
| `just check-containers` | `linux/amd64` and `linux/arm64` images built, including release musl binaries, and each passed all seven smoke checks; the bucket initialization restart reported `reconciled:0`, as expected, because its entries do not write the labels its API call changes |

`just check-sdk-clients`, which adds the Go `fakestorage` facade against the binary of the same source, passed too.

## Known gap: SDK runs under the fake-gcs-server profile

`GCS_MIMIC_COMPAT_PROFILE=fake-gcs-server just check-sdk` failed two Go cases, `TestObjectMediaLifecycle` and `TestMigrationAppScenarioBucketList`: they expected the fake-gcs-server profile and met the native one. The same command with `legacy-emulator` fails the same two cases on the source before these changes (`7016b7ee`), so the renaming did not cause it. Since `0.18.0`, `scripts/server-env.sh` removes every `GCS_MIMIC_*` variable before a runner starts its server, and `scripts/sdk-go.sh` passes no `--compat-profile`, so the variable reaches the tests but never the server. The SDK suites therefore have not exercised the fake-gcs-server profile since `0.18.0`; the Rust integration tests that start the server with `--compat-profile fake-gcs-server` still do.

## Not run

- `scripts/compare-reference-emulator.sh`, which needs the pinned fake-gcs-server checkout in `GCSM_REFERENCE_REPO`.
- The release workflow's builds of all eight native archives, which run in CI on a release tag; locally only Clippy type-checks the other targets.
- Tests that run only on Linux hosts.
