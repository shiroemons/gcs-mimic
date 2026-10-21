# Pre-push verification

Verified on 2026-10-22 using the accompanying Justfile, shared check script and
workflow changes. Runtime and API behavior are unchanged.

## Environment

The host is macOS arm64 with Rust 1.99.1, just 1.59.0 and Docker Engine 29.8.1.
The Go harness selects its pinned Go 1.27.3 toolchain and committed module graph.
Container checks use native linux/arm64 and emulated linux/amd64.

## Commands and results

| Command | Result |
| --- | --- |
| `just pre-push` | PASS, exit 0; Rust/shell, full Go SDK and both container architectures executed in order |
| `bash scripts/test-check.sh` | PASS; successful command order and immediate termination at each of five injected Cargo failures |
| Rust workspace tests through `scripts/check.sh` | PASS; 138 tests, 0 failed, 0 ignored |
| Rust formatting, Clippy and compatibility check through `scripts/check.sh` | PASS; locked inputs, offline lint/tests, generated views match |
| Go vet and uncached SDK tests through `scripts/sdk-go.sh` | PASS; endpoint validation, bucket lifecycle and object media lifecycle |
| `bash scripts/smoke-containers.sh` through the Justfile | PASS; linux/amd64 and linux/arm64, including CRUD, restart persistence, resource limits and signals |
| `cargo xtask dependencies check` | PASS, exit 0; all checked Rust, Cargo, Go, SDK, Actions and container inputs CURRENT |
| `actionlint .github/workflows/ci.yaml .github/workflows/dependency-check.yaml` | PASS, exit 0 |
| `shellcheck scripts/check.sh scripts/test-check.sh` | PASS, exit 0 |
| `just --list`, `just --dry-run pre-push`, `just --fmt --check` | PASS, exit 0 |
| `cargo xtask compatibility render` and `cargo xtask compatibility check` after the ledger update | PASS, exit 0; both generated views match |
| Markdownlint on README, development guide and this evidence | PASS; 0 issues, with only MD013 disabled for existing long tables, URLs and commands |

The Rust test total comprises 49 library, 6 bucket HTTP, 12 catalog, 10 foundation,
20 object HTTP, 21 object storage, 7 xtask library, 11 compatibility and 2 dependency
tests. The shell runner self-test is separate from that total.

## Scope and limitations

CI reuses the shared Rust/shell check while preserving its selected pinned/stable
toolchain. The Justfile selects the repository pin for local checks and removes
the Go test filter for the SDK suite. No Git hook, commit, push, release or remote
workflow run was performed. Freshness is a separate online check required at
slice start and before release.

The existing container harness removed only its isolated containers, networks
and volumes. The two image tags created by this run were removed afterwards;
no image was published and no shared-resource pruning was performed.

Remote GitHub Actions, native amd64 and Windows execution remain unverified.
The CI governance requirement therefore remains PARTIAL. No performance or
image-size improvement is claimed.
