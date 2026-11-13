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

## SDK runs under the fake-gcs-server profile

`GCS_MIMIC_COMPAT_PROFILE=fake-gcs-server just check-sdk` failed two Go cases, `TestObjectMediaLifecycle` and `TestMigrationAppScenarioBucketList`: they expected the fake-gcs-server profile and met the native one. The same command with `legacy-emulator` fails the same two cases on the source before these changes (`7016b7ee`), so the renaming did not cause it. Since `0.18.0`, `scripts/server-env.sh` removed every `GCS_MIMIC_*` variable before a runner started its server, and `scripts/sdk-go.sh` passed no `--compat-profile`, so the variable reached the tests but never the server, and no SDK run had exercised the profile since 2026-10-22.

`GCSM_SDK_COMPAT_PROFILE` now selects the profile of every server a run starts. The first full profile run then met differences in cases written since:

| Cases | Cause | Resolution |
| --- | --- | --- |
| Go cases that create buckets with default attributes, and others that create them through the client | Go Storage v1.70.0 sends `"location":"US"` by default, and the profile refused every create location but `US-CENTRAL1` since 2026-10-25 | The profile accepts `US` on create too (`a785f01b`) |
| Regional location cases in Go, Ruby, Node.js, Python and PHP | The profile refuses a regional create location | The cases check the 400 and that no bucket remains |
| Update cases in Go, Ruby, Node.js, Python and PHP | The profile ignores insert labels and preserves settings a PUT omits ([PUT contract](../slices/bucket-metadata-put.md)) | The cases check the profile's result |
| `TestReadETagValidators` | A profile bucket's JSON `etag` is not its HTTP validator ([validator contract](../slices/json-read-etag-validators.md)) | The case uses the response header |
| `TestObjectRewriteCopier`, `TestGRPCUpdateCopy` | Their second bucket names a location other than `US` | They use `US` under the profile |

On the final source, `GCSM_SDK_COMPAT_PROFILE=fake-gcs-server just check-sdk` and `just check-sdk` both pass all eight suites, `just check-sdk-clients` passes, and `cargo test --locked --workspace --no-fail-fast` passes 3427 tests with 0 failures and 1 ignored.

## Final source on main

On `d512e4ce`, the final commit of these changes:

| Check | Result |
| --- | --- |
| `just pre-push` | Passed in 17m33s: 3423 Rust tests in 16 binaries, 0 failed, 1 ignored; all eight SDK suites; both container architectures |
| Foundation checks workflow on the push to `main` | All 12 jobs passed, including `cargo test` on `x86_64-unknown-linux-musl`, `x86_64-pc-windows-msvc` and `aarch64-apple-darwin`, the Linux `rust` jobs on the pinned and current stable compilers, cross checks, containers, Testcontainers and the recorded differential |
| `scripts/compare-reference-emulator.sh` | Failed at the mimic PATCH assertion, in the same way on `7016b7ee` before these changes. Its bucket assertions had not followed the contracts since late October; after they were matched, the comparison passed ([verification](reference-comparison.md)). Up to where both stopped, the status differences between the reference and the fake-gcs-server profile are the same before and after these changes |

## Release

The release workflow built and tested all eight native archives on the `v0.20.0` commit and published the release; the [release gate evidence](local-release-gates.md#published-0200-source) records each run.
