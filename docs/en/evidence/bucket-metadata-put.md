# Bucket metadata PUT final integration evidence

<!-- markdownlint-configure-file {"MD013": false} -->

This is historical evidence for the original omission-reset contract. The
2026-10-27 [Ruby PUT revision](../slices/bucket-metadata-put.md#sdk-omission-preservation-verification)
preserves omitted settings; its current checks are recorded in that contract.
The container, reference and Docker guide runs below were not repeated for that revision.
The current native PUT block passes separately on 2026-10-27; see the contract.

Verified on **2026-10-23**. Final application, HTTP/catalog tests, Go SDK tests
and comparison script are source `c8d327a47446c0340796c87f06c6635059bc1db0`.
Heavy checks ran from `ff084a91f8b26e67f46b6c36a040538c521bae86`, which adds
contract/ledger wording only. A source diff against the former commit exited 0;
no application, test, script, dependency or lockfile changed during verification.
Earlier documentation and acceptance decomposition are retained from
`83c07c838433397cc2e856c2acc83a67662f58ec` and
`342e44730c79b0ed856b62e9dcf556fab534af30`.
The [contract](../slices/bucket-metadata-put.md) freezes the local replacement
policy and remaining provider questions; the [comparison](bucket-metadata-put-comparison.md)
separates emulator observations from Google compatibility.

## Inputs and environment

- macOS 27.0.1 (26A434), ARM64; native host `aarch64-apple-darwin`.
- Rust 1.99.1 (`58cf8f9c35b355199727627d07ae34a4e2846d67`, 2026-10-13),
  Cargo 1.99.1 (`5f94df478`), LLVM 23.1.1, edition 2024.
- Harness-selected Go 1.27.3; Storage v1.70.0, API v0.307.0 and OAuth2 v0.37.0.
  Explicit local endpoints, guarded transports and fixture credentials;
  inherited cloud credential and metadata-host variables were cleared for the
  full SDK run. No real bucket or metadata-server calls.
- Docker client/server 29.8.1, Linux ARM64 daemon; Compose v5.5.1 and buildx
  v0.37.1. ARM64 execution is native; AMD64 uses emulation.
- Unchanged SHA-256: Cargo.lock
  `a53256841d1e1f57d8a994237e1f0d903fb1bcb387989ac8b13da58084cb967d`;
  sdk-tests/go/go.mod
  `8fd0f4e9aebab325b6941543187e46c025ed9a50f1fab200ace7a0371f2a90e3`;
  sdk-tests/go/go.sum
  `e25a221b7d40e4ca4f5ad90c73084b6df714cd4878697cd702fe85a946fd8bb3`.
- [Discovery](https://storage.googleapis.com/$discovery/rest?version=v1)
  re-fetched on the check date: revision `20261013`, SHA-256
  `1f74b7a93b3d2872cc332fc20dafae1657fc255930ce336618bea3d0b4d95ae0`.
- Supplementary [Storage v2 proto](https://github.com/googleapis/googleapis/blob/14a161a98ef045be80c12ab69d924504a756f109/google/storage/v2/storage.proto),
  re-fetched at that commit: SHA-256
  `a0df6132967b97a89be248f1f24d277ee8df8f8d885db1d4af6249469ff0c1cf`.
- Reference v1.56.1, commit `896ece51e49eed0946089f3089f6343855c0d106`;
  the harness verified archive SHA-256
  `13fa1ed9cff0216baedaedb162b3cf37575bba955fb55fd4b76acb9b818494dc`.

Actual container inputs and locally built image indexes:

| Input | SHA-256 digest |
| --- | --- |
| `rust:1.99.1-slim-trixie` | `aab42391a986424e22d6a3ec14f37bc6dd3dacff4d457ca7cccd781e02297cc6` |
| `gcr.io/distroless/cc-debian13:nonroot` | `83b9b50586253821cf63ffcec87d3d94265a093ad30da232039af962dd8f4edf` |
| `docker/dockerfile:1` | `4edf897a3ffa55b89f906fc8cc78afdb3f1834cc9c7083565e611a8a7d5fe99e` |
| Test peer `curlimages/curl:8.23.0` | `11e64e28c5f58c8d93ee95fcf22ac055f48b0bc440e0ce7d8013bbb5ad6aa8ac` |
| Final local AMD64 smoke image | `f248bcc6af04488a0d0b12943ab3973b8aecdb0e8c607ef212b9341719771bb4` |
| Final local ARM64 smoke/guide image | `212913c74e405ea97e62ccf03f8b160b70f96f63da4b4af4c64114447585d870` |

These are local test artifacts; no image was published. Version, source, digest
and lockfile output is in the [checks log](bucket-metadata-put-checks.txt).
Each raw log declares replacement of local path prefixes on its first line;
other output is preserved.

## Commands and results

| Command | Result | Raw evidence |
| --- | --- | --- |
| `just check` | PASS, exit 0: 412 Rust tests, zero failures/ignored; fmt, clippy, compatibility, shell and release checks | [checks](bucket-metadata-put-checks.txt) |
| `just check-sdk` with inherited cloud credential and metadata-host variables cleared | PASS, exit 0: Go vet, 33 top-level tests and 73 subtests, zero failures/skips; package time 3.203s | [SDK](bucket-metadata-put-sdk-go.txt) |
| `GCS_MIMIC_REFERENCE_REPO=https://github.com/fsouza/fake-gcs-server bash scripts/compare-reference-emulator.sh` | PASS, exit 0: 4 targets × 294 requests = 1,176 records; 32 PUTs per target, both PUT and both PATCH assertion groups pass | [comparison](bucket-metadata-put-comparison.txt) |
| `just check-containers` | PASS, exit 0: AMD64/emulated and ARM64/native builds, ordinary and seeded Compose scenarios | [containers](bucket-metadata-put-containers.txt) |
| Getting-started native `sh` block, extracted unchanged and run with `bash -euxo pipefail` | PASS, exit 0: release build, media/XML checks, label merge/removal, class and GET equality | [guide](bucket-metadata-put-guide.txt) |
| Shared getting-started create/PATCH/PUT blocks against the native release binary | PASS, exit 0: replacement versus merge, omission resets, stale 412 and unchanged GET | [guide](bucket-metadata-put-guide.txt) |
| Shared getting-started create/PATCH/PUT blocks against the final ARM64 image | PASS, final exit 0: same assertions, then exact resource equality after Docker stop/start | [guide](bucket-metadata-put-guide.txt) |
| `cargo +1.99.1 xtask dependencies check` | UPDATE_AVAILABLE, exit 1: 30 inputs CURRENT; API client v0.307.0 → v0.308.0; pinned graph unchanged | [dependencies](bucket-metadata-put-dependencies.txt) |
| `cargo +1.99.1 xtask compatibility render`, `validate`, `check` | PASS, exit 0: 591 feature rows, 401 parameter locations and 357 fields; English/Japanese views regenerated | [checks](bucket-metadata-put-checks.txt) |
| Changed Markdown lint, whitespace and local-path scans | PASS: eight prose files and both generated matrices; default prose whitespace checks and path scan | [checks](bucket-metadata-put-checks.txt) |

Each heavy suite ran once on the final source. The Docker guide reused the
same ARM64 smoke image, without rebuilding, and pinned the selected host port
and advertised endpoint before creating the test bucket. Only the isolated
guide project's volume was removed at cleanup. One guide preparation attempt
selected the wrong build-log column and failed image inspection before starting
a container; its output remains in the log. Correct image selection then passed
all guide assertions and restart equality; no product or test change was needed.

The affected native and shared create/PATCH/PUT blocks ran; the unrelated full
onboarding journey was not rerun. The dependency result is an actionable stale
pin, not a passing freshness claim or a new compatibility exception. Updating
the API module graph and testing that update is separate maintenance work.

Prose retains its existing long-table/URL exemptions. Generated matrices use
MD013 disabled and MD024 limited to siblings because categories repeat under
different feature kinds. Docker progress output retains upstream trailing
spaces: raw-log whitespace checks disable only blank-at-eol, while prose,
JSON and generated matrices retain default whitespace checks.

## Delivery completion conditions

The three original replacement-scope conditions map to executed results:

| Completion condition | Outcome and evidence |
| --- | --- |
| Reconcile replacement, omitted/null/default and immutable/output-only inputs independently of PATCH | PASS: [contract](../slices/bucket-metadata-put.md), checked Discovery/proto pins and explicit SPEC-BUCKET-PUT questions |
| Shared atomic mutation/authorization; preserve objects, identity and IAM; invalid/stale requests preserve state | PASS: `bucket_put_*` HTTP/catalog cases and transaction-time revocation/incarnation checks in [checks](bucket-metadata-put-checks.txt); future versioning/hold effects and existing-history/hold preservation |
| Replacement-versus-merge, clear/default, strict denial, concurrency, GET/list/restart and applicable pinned SDK | PASS: HTTP/catalog tests, `TestBucketMetadataReplace`, `TestStrictAuthBucketMetadataReplace`, four-target [comparison](bucket-metadata-put-comparison.md), native/Docker [guide](bucket-metadata-put-guide.txt) and container checks |

## Ledger and remaining boundary

JSON-BUCKETS-013 has 13 passing scoped cases and three pending groups:
PUT-ADVANCED-FIELDS, PUT-ACL-BILLING-PROJECTION and PUT-PROVIDER-PARITY.
It remains PARTIAL with LOCAL_SIMULATION achieved fidelity. The delivered
replacement scope does not promote the complete method.
BEHAVIOR-BUCKET-002/004/012, BEHAVIOR-OBJECT-029,
BEHAVIOR-COMMON-006/011 and DESIGN-002/082 retain PARTIAL with passing linked
PUT cases; DESIGN-014 retains IMPLEMENTED for its existing local grant scope.
No earlier pending requirement or known deviation was removed.

Omitted labels/class/settings reset to declared local defaults. Null scalar
choices, ignored omitted/empty ACLs, immutable/unknown/output-only input rules,
no-effect revisions, timestamp/etag/error ordering and propagation remain
provider questions. SPEC-BUCKET-PUT explicitly retains getIpFilter and
setIamPolicy-for-empty-ACL permission questions; SPEC-017 separately retains
full-projection/authentication questions. Both reference backends have no bucket
PUT route and return JSON 404, preserving metadata and bytes.

Native-to-legacy metageneration-1 resources can hide labels/class; same-profile
round trips are covered. Default class is metadata, with no object rewrite,
physical placement or billing claim. Advanced writable settings, complete
IAM/ACL/UBLA/PAP, real-service parity, upstream emulator suites, Ruby/Rails,
power-loss durability and new performance/resource measurements remain outside
this delivery. No cloud resource, credential-dependent Docker step or publication
was used. The next slice is **Basic JSON contract**, starting with supported
response field selection and read ETag validators as separate small steps.

## SDK API client update

Verified on **2026-10-23** after the integration run above. Adopted
`google.golang.org/api` **v0.308.0** with the pinned Go **1.27.3** toolchain using
`go get google.golang.org/api@v0.308.0` and `go mod tidy` in `sdk-tests/go`.
Storage **v1.70.0**, OAuth2 **v0.37.0** and every indirect requirement remain
unchanged. Only the API requirement and its module checksums changed.
The verified module inputs and dependency snapshot are committed as
`4d18f4f7d999dacd0cc989f9f1f8e9c509a043c1`; application and test source are
unchanged by this update.
The earlier integration inputs and SDK logs retain their original versions;
this update supersedes the stale-pin result above. The refreshed
[dependency log](bucket-metadata-put-dependencies.txt) reports the current graph.

Official upstream checks at `2026-10-23T00:43:51Z`:

- The [module proxy info](https://proxy.golang.org/google.golang.org/api/@v/v0.308.0.info)
  identifies tag commit `5767912fa9673119eee358c714d9cb93f0ff1ba5` and module
  time `2026-10-22T17:26:34Z`; `@latest` returns the same release.
- The [official release](https://github.com/googleapis/google-api-go-client/releases/tag/v0.308.0)
  was published on **2026-10-22** at `17:26:56Z`, with `draft: false` and
  `prerelease: false`. Its change is automatic regeneration of Discovery clients.
- The [highest release's go.mod](https://proxy.golang.org/google.golang.org/api/@v/v0.308.0.mod)
  retracts only v0.258.0, not this version. `go list -m -json -retracted`
  reports no `Retracted` field for v0.308.0. Its minimum Go remains 1.26.0.
- The [v0.308.0 LICENSE](https://github.com/googleapis/google-api-go-client/blob/5767912fa9673119eee358c714d9cb93f0ff1ba5/LICENSE)
  remains BSD-3-Clause and is byte-identical to v0.307.0 (`cmp` exit 0).
  Both license files have SHA-256
  `110244b02140866ee37d17fa7449436a377ec3b85a481fbb208f4c87964382de`.
  The compressed module archive is 52,400,260 bytes.

Updated application-test input SHA-256:

| File | SHA-256 |
| --- | --- |
| `sdk-tests/go/go.mod` | `93a6051f221a20bf7586e846469c8012d0ea5e1d0512e9e9587772bb1d7d524e` |
| `sdk-tests/go/go.sum` | `1fe4e981f7239be32bec257908d18fea019b970e85ec86ffdcf559094d01fc38` |

| Command | Result |
| --- | --- |
| `cd sdk-tests/go && GOTOOLCHAIN=go1.27.3 go vet ./...` | PASS, exit 0 |
| `just check-sdk` with cloud credential/metadata-host variables cleared and pinned Go selected | PASS, exit 0: runner vet, 33 top-level tests and 73 subtests, zero failures/skips; package time 3.278s |
| `cargo +1.99.1 xtask dependencies check` | PASS, exit 0: all 31 inputs CURRENT, including API v0.308.0 |
| `cargo +1.99.1 xtask compatibility render` and `cargo +1.99.1 xtask compatibility check` | PASS, exit 0: 591 feature rows, 401 parameter locations and 357 fields; regenerated matrices have no content change |

The standalone vet, full SDK suite, dependency check and compatibility check each
ran once for this update. Complete output is in the
[SDK update log](bucket-metadata-put-sdk-update.txt) and separate refreshed
[dependency log](bucket-metadata-put-dependencies.txt). Local path prefixes are
replaced as declared on each log's first line. The SDK run starts isolated native
permissive and strict servers, uses guarded local transports and fixture tokens,
and makes no Google Cloud or metadata-server calls. No tests were changed.

Updated 41 current `sdk_evidence` entries on these 33 rows to API v0.308.0,
the new run date, command and raw artifact, only for test IDs present in this run:

- JSON-BUCKETS-001, JSON-BUCKETS-002, JSON-BUCKETS-003, JSON-BUCKETS-005,
  JSON-BUCKETS-008, JSON-BUCKETS-011 and JSON-BUCKETS-013.
- JSON-OBJECTS-004, JSON-OBJECTS-005, JSON-OBJECTS-007, JSON-OBJECTS-008 and
  JSON-OBJECTS-010.
- BEHAVIOR-BUCKET-012, BEHAVIOR-OBJECT-002, BEHAVIOR-OBJECT-010,
  BEHAVIOR-OBJECT-011, BEHAVIOR-OBJECT-012, BEHAVIOR-OBJECT-014,
  BEHAVIOR-OBJECT-021, BEHAVIOR-OBJECT-029 and BEHAVIOR-OBJECT-035.
- EXT-XML-012, EXT-XML-017, DESIGN-004, DESIGN-012, DESIGN-013, DESIGN-014,
  DESIGN-016, DESIGN-027, DESIGN-047, DESIGN-057, DESIGN-082 and UX-005.

Historical `verification` records, older SDK graph records, source references,
acceptance results, statuses and known deviations are unchanged. The current
SDK records describe this native run; earlier legacy-profile verification stays
in its original records. The dependency snapshot and current README, SDK,
development, getting-started and roadmap version/completion statements are
updated without rewriting historical evidence.

The [Dockerfile](../../../Dockerfile) copies Cargo inputs, Rust/xtask source and
licenses, builds the Rust application, then copies only the binary and license
bundle into the runtime. It neither copies nor builds `sdk-tests/go`; container
inputs and application source are unaffected. No container rebuild, guide run,
reference comparison, real-cloud differential test or additional Rust suite was
run for this harness-only update. Existing provider-parity and broad feature
limits remain open; the next implementation slice remains **Basic JSON contract**.

Markdown validation keeps the existing prose exemptions. The first default run
reported eight MD013 line-length issues in `docs/en/development.md`, whose long
paragraphs predate this version-only change. That file is checked separately
with only MD013 disabled; the other six changed Markdown files use their existing
rules. The initial diagnostic and final lint output are retained in the raw log.
