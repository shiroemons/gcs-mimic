# Public object read final integration evidence

<!-- markdownlint-configure-file {"MD013": false} -->

Verified source: `0baf5f3a0f1e916c335fee99186ba787edeffcd7` on **2026-10-22**.
The executable source and guide were final before the commands started.
Documentation and ledger updates follow those checks.
Each final-source command ran once; independent commands ran concurrently with
separate logs, local server ports and Compose projects.

## Inputs and environment

- Host: macOS, `aarch64-apple-darwin`; Rust 1.99.1
  (`58cf8f9c35b355199727627d07ae34a4e2846d67`, 2026-10-13),
  LLVM 23.1.1; Cargo 1.99.1 (`5f94df478`, 2026-08-27), edition 2024.
- Go 1.27.3, Storage `cloud.google.com/go/storage` v1.70.0,
  API `google.golang.org/api` v0.307.0 and OAuth2 v0.37.0.
  HTTP XML default reads and JSON use explicit local endpoints and fixture
  tokens; no Google credentials, cloud buckets or metadata-server calls.
- Docker Engine 29.8.1, Linux ARM64 daemon with kernel
  `7.0.14-linuxkit`; Compose 5.5.1. ARM64 executes natively and AMD64
  uses emulation on this ARM64 host.
- The application and SDK lockfiles are unchanged. SHA-256 values:
  `Cargo.lock`:
  `a53256841d1e1f57d8a994237e1f0d903fb1bcb387989ac8b13da58084cb967d`;
  `sdk-tests/go/go.mod`:
  `8fd0f4e9aebab325b6941543187e46c025ed9a50f1fab200ace7a0371f2a90e3`;
  `sdk-tests/go/go.sum`:
  `e25a221b7d40e4ca4f5ad90c73084b6df714cd4878697cd702fe85a946fd8bb3`.

The [slice contract](../slices/public-object-reads.md) defines request/response
fields, permission checks, errors, interactions and explicit local choices.
Official sources were checked on 2026-10-22; tagged sources and fetched-content
digests are in [Sources](../sources.md#public-object-read-source-pins).
The [XML status reference](https://docs.cloud.google.com/storage/docs/xml-api/reference-status)
distinguishes invalid/unsupported authorization headers (400
`InvalidAuthentication`) from missing credentials (401
`AuthenticationRequired`). The
[HEAD reference](https://docs.cloud.google.com/storage/docs/xml-api/head-object)
says Range adds Content-Range without otherwise changing HEAD, but does not
establish the local unsatisfiable `bytes */size` response. No real-service
differential request was made. The [slice-start freshness log](public-reads-dependencies.txt)
and final dependency log independently retain their actual check results.

## Commands and actual results

| Command | Result | Raw output |
| --- | --- | --- |
| `just check` | PASS, exit 0: 283 Rust tests (250 application/integration, 33 xtask), 0 failures/ignored; fmt, clippy, compatibility views and shell/release checks pass | [checks](public-object-reads-checks.txt) |
| `just check-sdk` | PASS, exit 0: Go vet, 18 top-level tests and 58 subtests, 0 failures/skips | [Go SDK](public-object-reads-sdk-go.txt) |
| `just check-containers` | PASS, exit 0: AMD64/emulated and ARM64/native, 4 passing ordinary/seeded Compose scenarios | [containers](public-object-reads-containers.txt) |
| `just check-dependencies` | PASS, exit 0: all 31 compiler, crate, SDK, action and container checks CURRENT; timestamp `1792663077` | [dependencies](public-object-reads-dependencies.txt) |
| All `sh` blocks in `docs/en/getting-started.md`, in order | PASS, exit 0: source-built ordinary/strict/seeded Compose, host/peer grants, native media and seed/range journeys | [guide](public-object-reads-guide.txt) |
| `cargo +1.99.1 xtask compatibility render` then `check` | PASS: 591 feature rows, 401 parameter locations and 357 schema fields; both generated views match | [checks](public-object-reads-checks.txt) |
| markdownlint-cli2, all 13 changed Markdown files | PASS: 0 issues after source-reference formatting corrections | [checks](public-object-reads-checks.txt) |

The guide ran directly with:

```sh
awk '/^```sh$/ { capture=1; next } /^```/ { capture=0 } capture { print }' docs/en/getting-started.md | bash -euxo pipefail
```

The native SDK run uses isolated strict and permissive servers and asserts the
actual default/XML path and method, explicit JSON read path, exact bytes,
generation/metageneration/size/content type/cache attributes, range offsets,
generation selectors and matching/mismatching conditions. Complete reads use
the official SDK's CRC32C validation; the byte-only success is not treated as
proof of every optional XML response header. Wire tests separately assert
content/custom metadata, hashes, stored length/encoding, Last-Modified and ETag.
An 8 MiB selected XML range remains on its original immutable generation during
overwrite and revocation; subsequent reads see the current grant and generation.

XML invalid/unregistered credentials and strict signature attempts return 400
`InvalidAuthentication`; anonymous denial returns 401
`AuthenticationRequired`; authenticated denial returns 403 `AccessDenied`.
JSON keeps 401 `required` / `authError` and 403 `forbidden`.
Anonymous JSON mutations and bucket/policy methods require each exact effective
permission. A read-only public grant does not authorize writes or policy changes.
Unknown XML queries and unsupported x-goog inputs return 501; one nonempty
SDK retry token and client telemetry are accepted without response replay.
Invalid raw bucket segments retain JSON-style unknown-route 404. Unrepresentable
or case-colliding custom metadata returns 501 without changing stored metadata.

The guide's permissive Go run passes 9 top-level tests and 36 subtests, with
9 strict-only top-level tests skipped because the strict endpoint is unset.
The subsequent strict Compose run passes all 9 selected top-level tests and
22 subtests with no skips. The isolated native SDK command separately passes
all 18 top-level tests and 58 subtests without skips; endpoint-specific skips
are not counted as passing strict evidence.

The strict host/peer journey verifies private denial before a grant, exact bytes
and HEAD generation/cache headers after allUsers, unchanged policy and public
reads after restart, registered-only access after replacing the public member
with allAuthenticatedUsers, and both anonymous/registered denial after revocation.
The source-built container check verifies non-root/read-only operation, configured
128 MiB/one CPU/64 PID/1024 descriptor bounds, host/peer requests, persistence,
explicit volume reset and SIGINT/SIGTERM. These are functional checks and
configured limits, not resource measurements. The guide's stopped seed demo
and exact generated smoke/seed image tags were removed afterward; cleanup output
is appended to the guide log.

Changed Markdown and both generated views pass markdownlint-cli2. MD013 is
disabled for existing long paragraphs/tables/commands, and MD024 uses
`siblings_only` for repeated category headings under separate feature kinds.
The checks log retains the initial 165 source-reference URL/table formatting
findings and the final clean 13-file lint result; those findings were corrected
without changing reference targets or feature statuses.

## Docker digests

All builds use the pinned input indexes below.

| Input | Digest |
| --- | --- |
| `rust:1.99.1-slim-trixie` | `sha256:d5928c68f8909c141df1bef47ac2c99c025fc9480d09870e08c3783b270eedec` |
| `gcr.io/distroless/cc-debian13:nonroot` | `sha256:83b9b50586253821cf63ffcec87d3d94265a093ad30da232039af962dd8f4edf` |
| `docker/dockerfile:1` | `sha256:4edf897a3ffa55b89f906fc8cc78afdb3f1834cc9c7083565e611a8a7d5fe99e` |
| Peer `curlimages/curl:8.23.0` | `sha256:11e64e28c5f58c8d93ee95fcf22ac055f48b0bc440e0ce7d8013bbb5ad6aa8ac` |

BuildKit's actual outputs, including distinct attestation-bearing indexes, are:

| Build | Image manifest | Local index |
| --- | --- | --- |
| Container smoke AMD64 | `sha256:3cea2e870f09e787626ec429daffe54d253befa4d1866fc8ba83194c383881cf` | `sha256:34c32bc794f9f23281f039419649f7df7c3a8ef45ad63612e24b8e04ac064606` |
| Container smoke ARM64 | `sha256:c26606c7e7dd541fb6273c70bf56e23b712e3f7a7d3c8de9f29bb48a6b9bb8f5` | `sha256:e5112ad5fb8146725033a8d5247268767dbce27b9b1dde744a1f8165955f20a4` |
| Guide ordinary Compose ARM64 | `sha256:c26606c7e7dd541fb6273c70bf56e23b712e3f7a7d3c8de9f29bb48a6b9bb8f5` | `sha256:72cf18ad1b08206d5a05bc2bebee2f2baa0a77c82912d08c45d72808e4236236` |
| Guide seed Compose ARM64 | `sha256:c26606c7e7dd541fb6273c70bf56e23b712e3f7a7d3c8de9f29bb48a6b9bb8f5` | `sha256:6415d3d5c66038a98e0a8e991b0fd22283db6b43df95a3b2afe0044520d6d58d` |

## Ledger judgement and limits

| Feature | Status | Reason |
| --- | --- | --- |
| DESIGN-004 | PARTIAL | Local anonymous/registered/public decisions and protocol errors pass; SPEC-017 real-service wire/error-order evidence and full restriction interactions remain pending |
| DESIGN-012 | IMPLEMENTED | All declared unconditional local allUsers permission, cache, revocation/restart and fail-closed restriction cases pass |
| DESIGN-013 | IMPLEMENTED | Registered-only matching, anonymous/unregistered denial, additive grants, private classification and guide cases pass |
| DESIGN-047 | IMPLEMENTED | Exact encoded path selection and permission checks pass in the declared shared-listener namespace profile; broader provider namespace/error-format parity remains explicit on XML methods |
| EXT-XML-012 | PARTIAL | The live-object GET subset passes; official input breadth, full header/error parity and dedicated XML resource verification remain pending |
| EXT-XML-017 | PARTIAL | The live-object HEAD subset and body suppression pass; unsupported official inputs and unsatisfiable Range/header/error parity remain pending |
| UX-005 | IMPLEMENTED | Strict host/peer and pinned Go default/XML plus JSON private/public grant, restart and revocation journeys pass |

No row is narrowed to hide a deviation. SPEC-012, SPEC-015, SPEC-017,
SPEC-XML-PREFIX/INPUTS/PATH-ERROR and SPEC-RANGE-FALLBACK remain open where they
affect complete API parity. The next deliverable is resumable upload.

Linux/Android uses SO_REUSEADDR for same-port restart. Other Unix platforms
retain exclusive binding and retry every 250 ms for at most 65 seconds, with one
stderr `bind_wait` event. macOS restart after connection-close traffic waited
about 30 seconds in the separately reported fix verification.
A live loopback listener is detected promptly; a wildcard bind probe can miss a
listener on a specific non-loopback IP, delaying EADDRINUSE to the deadline
without hijacking traffic. SIGINT/SIGTERM cancel bind_wait and release catalog
ownership. Windows behavior is unchanged. Windows cross-clippy and execution
on other non-Linux Unix systems were not performed. The coordinator separately
reported passing Linux clippy/foundation checks in the pinned Rust image for
`e281d9e`; that report is not counted as a command executed in this integration log.

Commit `6c92320` also includes documentation changes because a shared-index
staging collision combined concurrent edits. Final source and documentation
are reviewed together; this history note makes no additional passing-test claim.

No real-cloud differential tests, native AMD64 execution, remote CI,
publication, power-loss verification or performance/RSS/cgroup measurements
were performed. XML-specific large/slow/cancellation resource verification
remains pending; shared bounded-reader tests and the selected 8 MiB wire case
are not a resource benchmark. Virtual-host routing, XML writes/listing,
CSEK/user project, response overrides, HTTP If-* validators, signatures and
full ACL/UBLA/PAP state machines remain outside the delivered subset.
Raw logs redact local path prefixes only and preserve all other output.
