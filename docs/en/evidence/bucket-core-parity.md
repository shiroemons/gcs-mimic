# Bucket create, get and delete final integration evidence

<!-- markdownlint-configure-file {"MD013": false} -->

Verified on **2026-10-22**. The final Rust application and HTTP integration tests
are committed at `e17469279df6241779a8050250f2eeb7220f53c8`, including the
profile paging fix and generic configuration/reference names. Later ledger, generated
views and documentation changes do not change the application or dependency graph.
The [bucket contract](../slices/bucket-core-parity.md) and
[versioning/holds contract](../slices/bucket-versioning-holds.md) define the scope.

## Inputs and environment

- macOS 27.0.1 (26A434), ARM64; native Rust host `aarch64-apple-darwin`.
- Rust 1.99.1 (`58cf8f9c35b355199727627d07ae34a4e2846d67`, 2026-10-13),
  Cargo 1.99.1 (`5f94df478`), LLVM 23.1.1, edition 2024.
- Go 1.27.3; Storage `cloud.google.com/go/storage` v1.70.0,
  JSON API `google.golang.org/api` v0.307.0. Explicit local endpoints and fixture
  tokens; cloud credential variables cleared. No Google bucket or metadata calls.
- Docker client/server 29.8.1, Linux ARM64 daemon; Compose v5.5.1,
  buildx v0.37.1. ARM64 runs natively; AMD64 runs under emulation.
- Unchanged SHA-256: Cargo.lock
  `a53256841d1e1f57d8a994237e1f0d903fb1bcb387989ac8b13da58084cb967d`;
  sdk-tests/go/go.mod
  `8fd0f4e9aebab325b6941543187e46c025ed9a50f1fab200ace7a0371f2a90e3`;
  sdk-tests/go/go.sum
  `e25a221b7d40e4ca4f5ad90c73084b6df714cd4878697cd702fe85a946fd8bb3`.
- Pinned reference release v1.56.1, commit
  `896ece51e49eed0946089f3089f6343855c0d106`, archive SHA-256
  `13fa1ed9cff0216baedaedb162b3cf37575bba955fb55fd4b76acb9b818494dc`.

Container inputs actually used by the final checks:

| Input | Digest |
| --- | --- |
| `rust:1.99.1-slim-trixie` | `sha256:aab42391a986424e22d6a3ec14f37bc6dd3dacff4d457ca7cccd781e02297cc6` |
| `gcr.io/distroless/cc-debian13:nonroot` | `sha256:83b9b50586253821cf63ffcec87d3d94265a093ad30da232039af962dd8f4edf` |
| `docker/dockerfile:1` | `sha256:4edf897a3ffa55b89f906fc8cc78afdb3f1834cc9c7083565e611a8a7d5fe99e` |
| Test peer `curlimages/curl:8.23.0` | `sha256:11e64e28c5f58c8d93ee95fcf22ac055f48b0bc440e0ce7d8013bbb5ad6aa8ac` |

The locally built smoke image indexes were
`sha256:c0af31a2baf1ee0098354bf4ce137b698449dea8bf8765562ca974fdb7a6d565`
for AMD64 and
`sha256:982e5ba0c8e83d89d27c481f355f698e2cf44f2b57cee129dccccb82f1ba2b45`
for ARM64. These are local test artifacts, not published images.

## Commands and results

| Command | Result | Raw evidence |
| --- | --- | --- |
| `cargo test --locked --workspace`, fmt and clippy `-D warnings` after integration | PASS; final standard check includes all added regressions | [checks](bucket-core-parity-checks.txt) |
| `cargo xtask compatibility validate`, `render`, `check` | PASS; 591 feature rows; English/Japanese matrices regenerated | [checks](bucket-core-parity-checks.txt) |
| `just check` | PASS, exit 0: 378 Rust tests, no failures/ignored; fmt, clippy, ledger/generated views and shell/release checks | [checks](bucket-core-parity-checks.txt) |
| `cargo xtask dependencies check` | PASS, every actual input CURRENT; no upgrade or exception required | [freshness](bucket-core-parity-dependencies.txt) |
| `just check-sdk`, cloud variables cleared | PASS, 29 top-level tests, 65 subtests and Go vet; no failures/skips | [SDK](bucket-core-parity-sdk-go.txt) |
| `GCS_MIMIC_COMPAT_PROFILE=legacy-emulator just check-sdk` | PASS, same 29 top-level tests, 65 subtests and Go vet; no failures/skips | [SDK](bucket-core-parity-sdk-go.txt) |
| `just check-containers` | PASS, exit 0: AMD64/emulated and ARM64/native ordinary and seeded Compose scenarios | [containers](bucket-core-parity-containers.txt) |
| Every executable `sh` block in getting-started.md, in document order | PASS, exit 0: all 42 blocks, including bucket settings/history/hold, host/peer resumable, native/strict SDK and persistence/reset journeys | [guide](bucket-core-parity-guide.txt) |
| `bash scripts/compare-reference-emulator.sh` | PASS, 92 requests per target, 368 records; both references and both local profiles, no skipped target | [comparison](bucket-core-comparison.txt) |
| `npx --yes markdownlint-cli2 --config <MD013-disabled-config>` over the ten changed prose files | PASS; only MD013 disabled to preserve existing contract tables and source URLs | [checks](bucket-core-parity-checks.txt) |

Preliminary integration checks identified the native-only nonempty-delete SDK
expectation and the name/paging bounds; final checks include their regressions.
No negative case was skipped or weakened. Earlier local sandbox/timeout attempts
are not used as final passing evidence; the final source runs are captured below.

## Delivery completion conditions

The seven original conditions are retained here by content after moving
`bucket-core-parity` from the queue to existing scopes. Their executed scope is
separate from broad method/behavior completion.

| Original condition | Executed outcome and evidence |
| --- | --- |
| Pin create contract, official sources, naming/project/duplicate/resource/auth/errors; separate compatibility from permissive auth | PASS: [contract](../slices/bucket-core-parity.md), source digests and Discovery revision `20261013`; profile config/strict HTTP tests |
| Deliver name-only creation, source/native name differences, omitted project, invalid/duplicate state preservation and restart | PASS: `bucket_core_project_and_names`, `bucket_core_invalid_requests_preserve_state`, `bucket_core_source_input_shape`, `bucket_core_restart_settings`; [checks](bucket-core-parity-checks.txt) |
| Deliver basic GET and source fields/defaults, missing/auth behavior and create/GET consistency | PASS: `bucket_core_create_get_defaults`, `bucket_core_settings_round_trip`, `bucket_core_strict_permissions`, `bucket_core_profile_lookup_names_and_json_framing`; all four [comparison](bucket-core-comparison.md) create/GET bodies agree internally |
| Deliver empty delete, missing/nonempty/repeat/recreate and preserve all rejected-delete bytes | PASS: `bucket_core_missing_and_delete_recreate`, `bucket_core_nonempty_delete_preserves_objects`, history-only checks and exact payload comparison |
| Deliver source versioning/default-hold settings and actual effects, rather than metadata alone | PASS: `bucket_api_settings_drive_versioning_and_hold_effects` creates both settings through HTTP in both profiles, verifies inherited hold/rejection/release/history and permanent cleanup; eight versioning/four hold HTTP tests cover deeper effects/recovery |
| Execute equivalent wire and pinned SDK workflows, both source backends and documented normalization | PASS: 368 raw records, full native/profile SDK suites; [comparison inputs and differences](bucket-core-comparison.md) |
| Close baseline bugs, preserve raw results and broader pending rows; document intentional/strict/unsupported differences | PASS for the declared three-operation baseline: object/session/XML naming fixed and tested; every retained difference listed in [comparison](bucket-core-comparison.md#every-remaining-profile-difference) and [scope](../slices/bucket-core-parity.md#state-and-remaining-boundaries). Exact source/Google wire equivalence and full migration remain OPEN |

The final container/guide gates pass on the same application, lockfiles,
Docker inputs and runnable-guide source. Their evidence is the new final-source
capture above, rather than an inference from earlier slice logs. The isolated
stopped seed demo was removed after capture; no user project or volume was used.

## Acceptance, ledger and remaining boundaries

`JSON-BUCKETS-001`, `JSON-BUCKETS-002` and `JSON-BUCKETS-005` remain **PARTIAL**;
their existing pending policy, owner/ACL, advanced fields and provider cases are
unchanged. New profile/settings/deletion acceptance cases pass with HTTP test IDs,
current pinned SDK evidence and check dates in the single canonical ledger.

`BEHAVIOR-OBJECT-029` and `BEHAVIOR-BUCKET-012` are **PARTIAL**, target **EXACT**
unchanged, actual **LOCAL_SIMULATION**. All scoped local transition, generation,
paging, hold, permission, race, restart/crash and migration cases in the contracts
pass. Bucket metadata PATCH/PUT cannot yet toggle versioning/default hold;
catalog-boundary changed-setting tests are not presented as an HTTP setting API.
Provider propagation, selected-generation DELETE condition interpretation and
hold omission/null/PUT/error details remain explicit pending cases/deviations;
their API dependencies also remain PARTIAL. No scope or target was narrowed to
obtain IMPLEMENTED status.

Directly affected generation, versions/soft-delete listing, temporary/event-hold
and object insert/get/delete/list/PATCH/PUT rows gain only their real tested
subsets and remain PARTIAL. Stale claims that all history/holds are unsupported
are removed; temporary holds, retention, soft delete, full ACL/owner and provider
wire questions remain visible. See `SPEC-VERSION-DELETE-CONDITIONS` and
`SPEC-EVENT-HOLD-WIRE` in [gaps](../../../compatibility/gaps.json).

The profile never changes authentication: strict local tokens and transactional
project-plus-bucket grants apply in both profiles. Native keeps Google-oriented
naming and delete responses. Source-compatible buckets with native-rejected
names need the source profile for object access; switching profiles preserves
data. The new HTTP test covers media/resumable, metadata/media GET, list/delete,
XML GET/HEAD, native denial and restart recovery without direct catalog seeding.

The high-level pinned Go bucket Create serializer omits default hold, so enabled
hold creation uses the official JSON `Buckets.Insert` client before high-level
Writer/Attrs/Update/Delete. Source object wire rendering, lossless migration,
cloud infrastructure, full provider authentication, published images, real-cloud
checks, power-loss durability and new resource/performance claims are excluded.

The next implementation slice is **bucket metadata PATCH**: labels/default class,
merge/removal, atomic metageneration conditions, strict update permission and
persistence, after reconciling its precise input/default contract.
