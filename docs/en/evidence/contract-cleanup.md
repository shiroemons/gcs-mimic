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

## Not run

These run once on the release source and are not claimed here:

- `scripts/smoke-containers.sh` and the image builds. Its bucket initialization restart still expects `reconciled:0`, because its entries do not write the labels its API call changes; that expectation is unverified until it runs.
- The pinned SDK harnesses (`just check-sdk`), including `GCS_MIMIC_COMPAT_PROFILE=fake-gcs-server` runs and `scripts/compare-reference-emulator.sh`.
- `just pre-push`, the cross-target builds and the Linux-only tests.
