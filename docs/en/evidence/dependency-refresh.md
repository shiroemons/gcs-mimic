# Dependency refresh: 2026-10-27

<!-- markdownlint-configure-file {"MD013": false} -->

The final check at `2026-10-27T04:04:15Z` exited 0: all 39 inputs were `CURRENT`.
No update was deferred, no freshness exception remains, and no direct dependency was added.
Exact pins remain in the [snapshot](../../../compatibility/dependency-status.json) and lockfiles.

| Input | Previous | Adopted |
| --- | --- | --- |
| cc | 1.9.0 | 1.10.0 |
| libsqlite3-sys | 0.39.0 | 0.39.1, bundling SQLite 3.54.1 |
| smallvec | 1.16.3 | 1.16.4 |
| google.golang.org/api | v0.308.0 | v0.309.0 |
| faraday | 2.15.0 | 2.15.1 |
| google-apis-storage_v1 | 0.69.0 | 0.70.0 |
| pstore | 0.2.1 | 0.3.0 |
| docker/buildkit-syft-scanner:stable-1 | sha256:ae4f3b554449e7e25548e7d8ccc029d17357348e30c6e3df01b92bc93654d6a9 | sha256:873a7458c3c8ac8e32c7b796c10e1600b07b963d2aa000a72a2d97aaaa2d6315 |

## Validation performed on 2026-10-27

| Command or check | Result |
| --- | --- |
| `cargo check --workspace --all-targets --locked` | PASS |
| `just check` | PASS: 988 tests, no failures/skips; formatting, native/cross-target clippy, ledger, shell and offline publication checks |
| `BUNDLE_FROZEN=true bundle check` | PASS with Ruby 4.0.9/Bundler 4.0.23 before the SDK suite |
| `just check-sdk` | PASS: Go vet, 52 top-level tests and 85 subtests without failures/skips; Ruby self-check, migration, permissive/strict operations and V4 signatures |
| ARM64 scanner local OCI build and SPDX inspection | PASS: SPDX-2.3 generated with Syft v1.51.0 |
| `just check-dependencies` | PASS: exit 0, all 39 inputs `CURRENT` |

Application-container checks were omitted because Dockerfile inputs were unchanged; AMD64 execution was omitted as requested.
No benchmark, performance measurement, real-cloud test, HTTPS-mode SDK rerun, remote CI, push, publication or release was performed.
