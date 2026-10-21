# Go SDK harness verification: 2026-10-21

The original harness-only results are retained below. The integrated emulator now passes the full
emulator workflow with Go 1.27.3, storage v1.69.0 and API v0.306.0.

## Integrated execution

`bash scripts/sdk-go.sh` with the rustup proxy first in PATH passed vet and
both TestEmulatorEndpoint and TestBucketLifecycle, with no skipped tests.
The unchanged high-level storage client also passed against the read-only-root
Compose service from the getting-started flow. Requests stayed on the explicitly
validated emulator origin; no Google/metadata endpoint was contacted.

The native script selects a free loopback port first and passes that same origin
as `--api-base-url`. A competing bind makes startup fail instead of changing the
advertised endpoint. The pinned high-level `storage.BucketAttrs` does not expose
`selfLink`, so this Go workflow does not check that field; Rust wire tests check
the configured origin. The SDK type was checked on 2026-10-21 against
[bucket.go at v1.69.0](https://github.com/googleapis/google-cloud-go/blob/storage/v1.69.0/storage/bucket.go).

High-level Attrs/list request `projection=full`, so the emulator accepts it while
omitting unmodeled ACL/owner fields as an explicit compatibility deviation. The
SDK sends `pageToken=` initially, which selects the first page; malformed nonempty
tokens remain errors. These decisions and limitations are in the
[slice](../../docs/en/slices/json-bucket-crud.md) and
[integrated evidence](../../docs/en/evidence/bucket-crud.md).

## Commands and results

Rust commands below ran with `env -u RUSTUP_TOOLCHAIN` and the rustup proxy
directory first in `PATH`. The local mise shim otherwise re-injected 1.93.0 and
produced `error: rustc 1.93.0 is not supported ... requires rustc 1.99.1`.
`rustc -Vv` through the proxy reported 1.99.1 (58cf8f9c3 2026-10-13).

Go commands ran in `sdk-tests/go` with `GOTOOLCHAIN=auto`,
`GOMODCACHE=$TMPDIR/gcs-mimic-go-mod` and `GOCACHE=$TMPDIR/gcs-mimic-go-build` so the
checked toolchain and module downloads did not modify the user's Go cache.
The original Go 1.24.5 successfully downloaded Go 1.27.3 (darwin/arm64) after
network access was allowed. Official sources and pins are in [README](README.md)
and [the dependency snapshot](../../compatibility/dependency-status.json).

| Command | Result |
| --- | --- |
| `go version` | `go version go1.27.3 darwin/arm64` |
| `go vet ./...` | PASS |
| `go test -count=1 -v ./...` | PASS: endpoint guard; SKIP: bucket integration |
| `cargo test --locked -p xtask` | PASS: 18 tests |
| `cargo fmt --check` | PASS |
| `cargo clippy --locked -p xtask --all-targets -- -D warnings` | PASS |
| `cargo clippy --locked --all-targets -- -D warnings` | PASS |
| `cargo test --locked` | PASS: 8 foundation tests, with local TCP bind permitted |
| `cargo xtask dependencies check` | PASS: exit 0; full output below |
| `bash -n scripts/sdk-go.sh` | PASS |
| `shellcheck scripts/sdk-go.sh` | PASS |
| `git diff --check` | PASS |

Integration skip output:

```text
=== RUN   TestBucketLifecycle
GCS_MIMIC_ENDPOINT is unset; bucket integration requires an explicit local emulator
--- SKIP: TestBucketLifecycle (0.00s)
PASS
```

The first full foundation test invocation inside the sandbox failed seven TCP
tests with `Os { code: 1, kind: PermissionDenied, message: "Operation not permitted" }`
at loopback bind. The same unchanged tests passed outside that sandbox. No test
was skipped or weakened to address that environment restriction.

## Online dependency check output

Checked at Unix timestamp 1792575474, exit 0:

```text
rust: CURRENT current=1.99.1 latest=1.99.1
http-body-util: CURRENT current=0.1.5 latest=0.1.5
hyper: CURRENT current=1.12.0 latest=1.12.0
hyper-util: CURRENT current=0.1.21 latest=0.1.21
serde_json: CURRENT current=1.0.154 latest=1.0.154
tokio: CURRENT current=1.54.0 latest=1.54.0
actions/checkout: CURRENT current=3d3c42e5aac5ba805825da76410c181273ba90b1 latest=3d3c42e5aac5ba805825da76410c181273ba90b1
actions/setup-go: CURRENT current=b7ad1dad31e06c5925ef5d2fc7ad053ef454303e latest=b7ad1dad31e06c5925ef5d2fc7ad053ef454303e
docker library/rust:1.99.1-slim-trixie: CURRENT current=sha256:00544ccc9431a2fe93e65185d21381b0a039342f5cefcf84fbddac273c9e573d latest=sha256:00544ccc9431a2fe93e65185d21381b0a039342f5cefcf84fbddac273c9e573d
docker library/rust:slim-trixie: CURRENT current=sha256:00544ccc9431a2fe93e65185d21381b0a039342f5cefcf84fbddac273c9e573d latest=sha256:00544ccc9431a2fe93e65185d21381b0a039342f5cefcf84fbddac273c9e573d
Debian stable codename: CURRENT current=trixie latest=trixie
docker library/debian:trixie-slim: CURRENT current=sha256:62bd423aeee912639ca8ee681c2b762cb3e25137e0a92b20be455898a9f034ad latest=sha256:62bd423aeee912639ca8ee681c2b762cb3e25137e0a92b20be455898a9f034ad
docker docker/dockerfile:1: CURRENT current=sha256:4edf897a3ffa55b89f906fc8cc78afdb3f1834cc9c7083565e611a8a7d5fe99e latest=sha256:4edf897a3ffa55b89f906fc8cc78afdb3f1834cc9c7083565e611a8a7d5fe99e
Go language baseline: CURRENT current=1.27.0 latest=1.27.0
Go toolchain: CURRENT current=1.27.3 latest=1.27.3
cloud.google.com/go/storage: CURRENT current=v1.69.0 latest=v1.69.0
google.golang.org/api: CURRENT current=v0.306.0 latest=v0.306.0
```

## Decisions and remaining verification

Use `go 1.27.0` with `toolchain go1.27.3`: Go removes an identical patch-level
toolchain directive and then refuses ordinary tests until tidy is applied. The
checker compares the language series and actual stable patch separately.
setup-go's pinned source selects the toolchain directive when `GOTOOLCHAIN` is
unset/auto. No Rust dependency was added. The generated API client requires an
auth pseudo-version transitively; the snapshot records this upstream constraint.

At the original harness-only checkpoint, the bucket server and `--data-dir` support were unavailable, so
`scripts/sdk-go.sh` and `TestBucketLifecycle` have not run end to end. The
integration task must run the script after merging the bucket implementation,
record real SDK evidence and update JSON-BUCKETS-001/002/005/006 in the ledger.
Remote CI, scheduled workflow execution, XML, gRPC, objects and ACL/IAM were not
tested. No real Google Cloud API or metadata endpoint was contacted by tests.
