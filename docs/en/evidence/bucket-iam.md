# Bucket IAM final integration evidence

<!-- markdownlint-configure-file {"MD013": false} -->

Verified on **2026-10-22** against source commit
`7fadac2d5c15fd2534f915d90295f5d314486cb1` (merged server, Go tests and guide).
Subsequent changes in this evidence delivery affect documentation and the ledger only.
Each heavy command below ran once against that final executable source.

## Inputs and environment

- Host: macOS, `aarch64-apple-darwin`; Rust 1.99.1
  (`58cf8f9c35b355199727627d07ae34a4e2846d67`, 2026-10-13), LLVM 23.1.1;
  Cargo 1.99.1 (`5f94df478`, 2026-08-27), edition 2024.
- Go: 1.27.3, `darwin/arm64`, selected by the committed module's toolchain directive.
  Storage client `cloud.google.com/go/storage` v1.70.0, API client
  `google.golang.org/api` v0.307.0, OAuth2 v0.37.0; JSON HTTP transport,
  explicit local endpoints and fixture bearer tokens. No Google credentials,
  cloud buckets or metadata-server requests were used.
- Docker Engine/Desktop: 29.8.1 / 4.93.0; Linux ARM64 daemon,
  kernel `7.0.14-linuxkit`; Compose 5.5.1. Linux ARM64 execution is native;
  Linux AMD64 execution is emulated on this ARM64 host.
- `Cargo.lock`, `sdk-tests/go/go.mod` and `sdk-tests/go/go.sum` remained unchanged.
  SHA-256 values are respectively
  `a53256841d1e1f57d8a994237e1f0d903fb1bcb387989ac8b13da58084cb967d`,
  `8fd0f4e9aebab325b6941543187e46c025ed9a50f1fab200ace7a0371f2a90e3`, and
  `e25a221b7d40e4ca4f5ad90c73084b6df714cd4878697cd702fe85a946fd8bb3`.

The [slice contract](../slices/bucket-iam.md) lists the official request, response,
permission, role, principal and status references rechecked on 2026-10-22.
The live [Storage v1 Discovery](https://storage.googleapis.com/$discovery/rest?version=v1)
was fetched again: revision `20261013`, SHA-256
`1f74b7a93b3d2872cc332fc20dafae1657fc255930ce336618bea3d0b4d95ae0`,
identical to the pinned contract. It requires `bindings` on set and a requested
policy version of at least 1. The official
[default binding description](https://docs.cloud.google.com/storage/docs/access-control/iam-roles#modifiable-behavior)
confirms the non-UBLA bucket convenience bindings used by the fixture.

## Commands and actual results

| Command | Result | Raw output |
| --- | --- | --- |
| `just check` | PASS, exit 0: 260 Rust tests (227 application/integration, 33 xtask), 0 failures/ignored; fmt, clippy, generated compatibility views and shell/release checks pass | [checks](bucket-iam-checks.txt) |
| `just check-sdk` | PASS, exit 0: Go vet, 13 top-level tests and 38 subtests, 0 failures/skips, isolated permissive and strict native servers | [Go SDK](bucket-iam-sdk-go.txt) |
| `just check-containers` | PASS, exit 0: Linux AMD64/emulated and ARM64/native; both ordinary and seeded Compose checks on both architectures | [containers](bucket-iam-containers.txt) |
| `just check-dependencies` | PASS, exit 0: all 31 compiler, crate, SDK, action and container checks CURRENT; check timestamp `1792657427` | [dependencies](bucket-iam-dependencies.txt) |
| All `sh` blocks in `docs/en/getting-started.md`, in order | PASS, exit 0: source-built Compose, native release binary, strict IAM journey, native seed/range and seeded Compose | [guide](bucket-iam-guide.txt) |
| `cargo xtask compatibility render` then `cargo xtask compatibility check` | PASS: 591 feature rows, 401 parameter locations and 357 schema fields; both generated views match | [checks](bucket-iam-checks.txt) |

The guide was executed directly with:

```sh
awk '/^```sh$/ { capture=1; next } /^```/ { capture=0 } capture { print }' docs/en/getting-started.md | bash -euxo pipefail
```

This runs the source-built container wherever the guide specifies it. The first
Go command against permissive Compose passes 8 top-level tests and skips the 5
strict-only tests because `GCS_MIMIC_STRICT_ENDPOINT` is unset. The subsequent
`go test -count=1 -v -run '^TestStrictAuth' ./...` against strict Compose passes all
5 strict tests with no skips. The isolated `just check-sdk` run separately passes
the complete 13-test suite without skips. These endpoint-specific skips are not
counted as strict passing evidence in the permissive run.

Changed Markdown and generated views pass `markdownlint-cli2`. MD013 is disabled
for the existing long tables/commands; MD024 uses `siblings_only` because the
generated matrix repeats category headings under different feature-kind parents.

Strict Compose reports `{"auth_mode":"strict","identities":4}`. The guide checks
default convenience bindings, project admin access, a bucket-only object viewer,
exact downloaded bytes and listing, denial for other buckets/creation/policy
management, stale-etag 412 without lost grants, revocation, and the same policy
etag after stop/start. It also checks the encoded public and dot-segment URLs
and the public URL's 404, then returns to permissive mode. Native SIGTERM is
followed by `wait` exactly as documented; both native processes exit successfully.
No address-in-use failure occurred, so the guide's shutdown steps need no fix.
The standalone seed Compose demonstration stops as documented; its dedicated
project volume/network/container and exact image tag were then removed, with
cleanup output appended to the guide log.

The container check verifies UID/GID 10001, read-only root, 128 MiB memory/one
CPU/64 PIDs/1024 descriptors, isolated probes, host and peer-container requests,
bucket/object CRUD, stop/start and recreate persistence, reset, SIGINT/SIGTERM,
and read-only seed import with mutation/deletion persistence and fresh-volume
reimport. These are configured limits and functional checks, not resource benchmarks.

## Docker digests

All builds use the Dockerfile's pinned input indexes:

| Input | Digest |
| --- | --- |
| `rust:1.99.1-slim-trixie` | `sha256:d5928c68f8909c141df1bef47ac2c99c025fc9480d09870e08c3783b270eedec` |
| `gcr.io/distroless/cc-debian13:nonroot` | `sha256:83b9b50586253821cf63ffcec87d3d94265a093ad30da232039af962dd8f4edf` |
| `docker/dockerfile:1` | `sha256:4edf897a3ffa55b89f906fc8cc78afdb3f1834cc9c7083565e611a8a7d5fe99e` |
| Test peer `curlimages/curl:8.23.0` | `sha256:11e64e28c5f58c8d93ee95fcf22ac055f48b0bc440e0ce7d8013bbb5ad6aa8ac` |

The [dependency snapshot](../../../compatibility/dependency-status.json) retains
the platform manifests under those indexes; the current freshness log confirms
the pinned values remain CURRENT. BuildKit's actual output manifests are:

| Build | Image manifest | Local index including build attestation |
| --- | --- | --- |
| Container smoke AMD64 | `sha256:f3f2ba855bc1af3d10017469ec9ca15aedb2e72116092c402fa9b94ef4a178e7` | `sha256:e874cdff6b2bd089932e6e43a9210bce45be8105dd18d18a68734fc128139e52` |
| Container smoke ARM64 | `sha256:8894acc9d26777b2d639dc2da9c89b8803767ddb8c2e2ed30f5f7d7a08de4a24` | `sha256:1fad82fba887c9439fddd9b6012c981035956d07f7150e59667447aeda5c29d4` |
| Guide ordinary Compose ARM64 | `sha256:8894acc9d26777b2d639dc2da9c89b8803767ddb8c2e2ed30f5f7d7a08de4a24` | `sha256:b3f1dfc7399b183f79517154517bbb900b987d84cfe256c8abfdb13bd00fd08c` |
| Guide seed Compose ARM64 | `sha256:8894acc9d26777b2d639dc2da9c89b8803767ddb8c2e2ed30f5f7d7a08de4a24` | `sha256:412fb1bd6c95ac839ea8a8b5734c4053458401d94e9343cdb1ce5d42bc457ba2` |

## Ledger judgement and limits

| Feature | Status | Reason |
| --- | --- | --- |
| JSON-BUCKETS-003 | PARTIAL | Scoped policy reads pass; unsupported policy/billing semantics and SPEC-017 wire/missing-bucket parity remain, as does duplicate-binding response normalization parity |
| JSON-BUCKETS-011 | PARTIAL | Scoped replacements, limits, rejection and CAS pass; unsupported principal/role/condition/billing/ACL interactions and normalization/malformed-input/error parity remain |
| DESIGN-014 | IMPLEMENTED | Every declared unconditional predefined-role bucket-grant enforcement case passes across existing JSON routes, including transaction-time revocation and overwrite/recreation races |
| DESIGN-016 | PARTIAL | Additive local project-plus-bucket grants pass; organization/folder hierarchy, basic-role convenience membership and broader policy evaluation remain missing |
| DESIGN-027 | IMPLEMENTED | Atomic compare-and-swap, stale-update preservation, unconditional overwrite, fresh revisions, persistence and no etag reuse after recreation pass |

No broad API or hierarchy row was narrowed to discard missing cases. The ledger
retains concrete pending acceptance cases and the evidence needed to close them.
SPEC-017 stays open, and new IAM parity uncertainties stay open in `gaps.json`.
Malformed etag mixed-alphabet acceptance and non-zero-trailing-bit rejection are
stated local behavior; real-service parity is unverified. The missing
`policy_revision` singleton startup invariant is enforced but lacks a dedicated
fixture. Neither limit invalidates the passing grant-enforcement or atomic-CAS
cases; neither is claimed as passing cloud wire/startup-fixture evidence.

No real-cloud differential tests, native AMD64 execution, remote CI, publication,
power-loss verification or performance/RSS/cgroup measurements were performed.
Public grants/XML reads, ACLs, conditions, custom/basic roles, signed URLs and
`testIamPermissions` remain excluded from the delivered capability. The next
capability is public object reads. Raw logs redact only local path prefixes and
retain command output, including progress lines and intentional endpoint-specific skips.
