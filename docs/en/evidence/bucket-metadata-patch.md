# Bucket metadata PATCH final integration evidence

<!-- markdownlint-configure-file {"MD013": false} -->

Verified on **2026-10-23**. Application source is
`2b4441cef21370f95cfb3506da1cbf1ebda09787`; final SDK/comparison source is
`f48651b7991ccc90a588797f2b2fb09935754209`, integrated by
`ce521fd213b389c63c48cd29d67395cdb21aac38`. Final heavy checks ran after draft
documentation commit `24364d845b186a56fc3c596de29b7a4b21b463ae`.
Later changes affect only ledgers, generated views, prose and evidence.
The [contract](../slices/bucket-metadata-patch.md) freezes test IDs, profile rules
and remaining Google questions. The [comparison](bucket-metadata-patch-comparison.md)
records the pinned reference and SDK serialization separately from whole-method
compatibility. No source, test, dependency or lockfile changed during integration.

## Inputs and environment

- macOS 27.0.1 (26A434), ARM64; native host `aarch64-apple-darwin`.
- Rust 1.99.1 (`58cf8f9c35b355199727627d07ae34a4e2846d67`, 2026-10-13),
  Cargo 1.99.1 (`5f94df478`), LLVM 23.1.1, edition 2024.
- Go 1.27.3; Storage v1.70.0, API client v0.307.0. Explicit local endpoints and
  fixture credentials; inherited cloud credential variables cleared for the SDK
  harness. No real bucket or metadata-server calls.
- Docker client/server 29.8.1, Linux ARM64 daemon; Compose v5.5.1 and buildx
  v0.37.1. ARM64 execution is native; AMD64 uses emulation.
- Unchanged SHA-256: Cargo.lock
  `a53256841d1e1f57d8a994237e1f0d903fb1bcb387989ac8b13da58084cb967d`;
  sdk-tests/go/go.mod
  `8fd0f4e9aebab325b6941543187e46c025ed9a50f1fab200ace7a0371f2a90e3`;
  sdk-tests/go/go.sum
  `e25a221b7d40e4ca4f5ad90c73084b6df714cd4878697cd702fe85a946fd8bb3`.
- Discovery revision `20261013`, SHA-256
  `1f74b7a93b3d2872cc332fc20dafae1657fc255930ce336618bea3d0b4d95ae0`,
  checked on 2026-10-23; supplementary googleapis commit
  `14a161a98ef045be80c12ab69d924504a756f109`.
- Reference v1.56.1, commit `896ece51e49eed0946089f3089f6343855c0d106`, archive
  SHA-256 `13fa1ed9cff0216baedaedb162b3cf37575bba955fb55fd4b76acb9b818494dc`.

Actual container inputs and locally built image indexes:

| Input | SHA-256 digest |
| --- | --- |
| `rust:1.99.1-slim-trixie` | `aab42391a986424e22d6a3ec14f37bc6dd3dacff4d457ca7cccd781e02297cc6` |
| `gcr.io/distroless/cc-debian13:nonroot` | `83b9b50586253821cf63ffcec87d3d94265a093ad30da232039af962dd8f4edf` |
| `docker/dockerfile:1` | `4edf897a3ffa55b89f906fc8cc78afdb3f1834cc9c7083565e611a8a7d5fe99e` |
| Test peer `curlimages/curl:8.23.0` | `11e64e28c5f58c8d93ee95fcf22ac055f48b0bc440e0ce7d8013bbb5ad6aa8ac` |
| Final local AMD64 smoke image | `f5e0f7c17c55f1dc5bbdfee57cc9cde7308e30df533f9cc37c7f3e520dc55b13` |
| Final local ARM64 smoke/guide image | `db2eeba656233549d4ef2c043a671bff5f37d1a680a43eb674b3468f048764f3` |

These are local test artifacts; no image was published. Full environment and
lockfile output is appended to the [checks log](bucket-metadata-patch-checks.txt).

## Scope and acceptance

Labels merge, key-null removal, whole-map null clearing and empty-map preservation;
supported default class; versioning/default holds and their future object effects;
atomic match/not-match conditions; metageneration/etag/clock changes; strict update
permission, transaction-time revocation and durable GET/list consistency.
The feature ledger decomposes these cases and retains pending advanced-field,
ACL/billing/full-projection and provider-parity cases. JSON-BUCKETS-008 remains
PARTIAL. No whole-method promotion follows from a scoped workflow checkmark.

## Commands and results

| Command | Result | Raw evidence |
| --- | --- | --- |
| `just check` | PASS, exit 0: 396 Rust tests, zero failures/ignored; fmt, clippy, compatibility and shell/release checks | [checks](bucket-metadata-patch-checks.txt) |
| `just check-dependencies` | PASS, exit 0: every actual tracked input CURRENT, no exception or update | [dependencies](bucket-metadata-patch-dependencies.txt) |
| `just check-sdk` with inherited cloud credential variables cleared | PASS, exit 0: Go vet, 31 top-level tests and 73 subtests, zero failures/skips; package time 5.346s | [SDK](bucket-metadata-patch-sdk-go.txt) |
| `just check-containers` | PASS, exit 0: AMD64/emulated and ARM64/native builds, ordinary and seeded Compose scenarios | [containers](bucket-metadata-patch-containers.txt) |
| Getting-started native `sh` block, extracted unchanged and run with `bash -euxo pipefail` | PASS, exit 0: release build, media/XML checks, label add/removal, class change and GET equality | [guide](bucket-metadata-patch-guide.txt) |
| Getting-started create and PATCH `sh` blocks against the final ARM64 image | PASS, final exit 0: merge/removal, class, stale 412 with unchanged resource, then stop/start persistence | [guide](bucket-metadata-patch-guide.txt) |
| `bash scripts/compare-reference-emulator.sh` | PASS: inherited committed run, 4 targets × 185 requests = 740 records, both assertion groups pass | [comparison](bucket-metadata-patch-comparison.txt) |
| `cargo xtask compatibility validate`, `render`, `check` | PASS: 591 feature rows, 401 parameter locations and 357 fields; English/Japanese views regenerated | [checks](bucket-metadata-patch-checks.txt) |
| `npx --yes markdownlint-cli2 --config <MD013-disabled-config>` on changed prose; generated matrices with `MD024.siblings_only=true` | PASS; MD013 disabled for existing long tables/URLs/commands; matrix categories may repeat under different feature kinds | [checks](bucket-metadata-patch-checks.txt) |
| Changed-file path scan and scoped `git diff --check` | PASS: default whitespace rules for prose/JSON/views; raw logs preserve upstream trailing padding with `blank-at-eol` disabled | [checks](bucket-metadata-patch-checks.txt) |

Each heavy suite ran once on the final merged source. The comparison run is
reused from its committed evidence; a diff against comparison checkout
`3035d23f7b3a9c85a786441fc07c961d037c8eca` confirms identical application,
HTTP/catalog tests, PATCH SDK test and comparison script. Its raw file retains
372 PATCH-related records plus its own complete SDK output.

The first Docker guide preparation omitted the documented selected-port pinning
step. All PATCH assertions passed, but the extra post-restart request used the
old dynamically allocated port and failed with curl exit 7, “Failed to connect”.
The guide-only retry applied `GCS_MIMIC_PORT=${address##*:}` and the documented
advertised endpoint before recreation; the same image then passed PATCH and
post-restart resource equality. Both attempts remain in the raw log. No product
code or test changed, no negative test was weakened, and no heavy suite reran.

The guide verification executes the affected create/PATCH and native sections,
plus Docker restart persistence. The full unrelated onboarding journey was not
rerun; its earlier evidence remains separate.

The initial Markdown pass included both generated matrices with global MD024
duplicate-heading checks and reported eight existing category-title repetitions.
The prose passes with only MD013 disabled; generated matrices pass with MD024
limited to sibling headings because behavior and project-requirement groups
intentionally share category titles. No generator or generated text was edited
by hand. Both lint outputs are retained in the checks log.

Docker Compose emits trailing spaces in its progress lines. Raw logs preserve
that output, so their whitespace check disables only `blank-at-eol`; ordinary
prose, JSON and generated views retain the default whitespace check. The initial
unscoped staged check reported this padding; it is not removed from raw evidence.

## Delivery completion conditions

The three original order-6 conditions map to the following executed scope:

| Completion condition | Outcome and evidence |
| --- | --- |
| Freeze current-source labels/class PATCH, omitted/null/removal, immutable/output-only and unsupported fields | PASS: [contract](../slices/bucket-metadata-patch.md), Discovery pin, [source check](../sources.md#bucket-metadata-patch-source-check), explicit SPEC-BUCKET-PATCH and reference differences |
| Atomic merge/removal, metageneration conditions, strict update permission, revision/clock and failure preservation | PASS: `bucket_patch_*` HTTP/catalog cases and transaction-time revocation test in [checks](bucket-metadata-patch-checks.txt); versioning/default-hold changes drive future object effects |
| Wire/SDK labels, stale/concurrent updates, GET/list and restart with field evidence | PASS: HTTP/catalog cases, `TestBucketMetadataUpdate`, `TestStrictAuthBucketMetadataUpdate`, [SDK](bucket-metadata-patch-sdk-go.txt), both local/reference profiles in the [comparison](bucket-metadata-patch-comparison.md), native/Docker [guide](bucket-metadata-patch-guide.txt) |

## Ledger and remaining boundary

JSON-BUCKETS-008 has 13 passing scoped cases and three explicit pending groups:
advanced writable fields, ACL/billing/full-projection semantics, and provider
edge parity. It remains PARTIAL with LOCAL_SIMULATION achieved fidelity.
BEHAVIOR-BUCKET-002/004/012, BEHAVIOR-OBJECT-029,
BEHAVIOR-COMMON-006/011 and DESIGN-002/082 gain passing PATCH cases and remain
PARTIAL because their earlier broader pending cases/deviations still apply.
DESIGN-014 retains IMPLEMENTED for its existing unconditional local grant scope;
the new update permission and revocation cases pass within that same scope.
No pending requirement was removed to promote a row.

Versioning/default holds, labels and supported class are effective in both local
profiles. Intentional reference differences include preserving omitted booleans,
real revision fields after PATCH, durable versioning beyond the reference
filesystem backend, and fail-closed unsupported settings. Whole-map null and
other local choices are not direct-service observations. Default class metadata
does not reproduce physical placement or billing. Native-to-legacy initial
resource carry-over and insert location-case limits remain documented.
Full Google authentication, ACL/UBLA/PAP, nonempty lifecycle rules, CORS,
retention, power-loss durability, upstream emulator suites and new resource or
performance claims remain outside this delivered subset. No real-cloud resource
or release publication was used. The next slice is **bucket metadata PUT**,
order 7, with independently reconciled replacement/default semantics.
