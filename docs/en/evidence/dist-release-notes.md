# Initial release preparation evidence

<!-- markdownlint-configure-file
{"MD013": {"code_blocks": false, "tables": false}}
-->

Checked on **2026-10-24**, using preparation baseline `71883c0`. The historical
preparation comparison below matched the protected inputs at
`900393902a1a7a58ac34d5a77d37861105d106ef`. After rebase onto main `280a84c`,
protected inputs differ as recorded below; this evidence does not select a new
0.1.0 behavior baseline or treat those differences as permitted pin updates.
Configuration and import contracts do not establish released implementations.
The [contract](../slices/dist-release-notes.md) records the scope and handoff.

This is a **pre-tag preparation snapshot**, including its tables and the
generated feature matrix linked from the notes. Before tagging, local gates,
the final Release body and the exact candidate's source identity must pass;
recording their failures here does not waive those gates. Tag source identity
remains pending until that candidate is checked, and tag execution remains
pending until the tag runs. `dist-post-publication` records the actual tag run
IDs and final gate results in the main-branch ledger; the tag-pinned snapshot is
not rewritten.

## Documentation review

`release-notes-scope` and `release-doc-consistency` cover
[0.1.0 notes](../releases/0.1.0.md), both READMEs and the getting-started
specification. Review checked the feature ledger, current JSON config parser,
actual `config/development.json`, publication workflows, release/platform policy
and existing storage/SDK/browser evidence. The reduced Compose example preserves
the checked-in image CMD (all-interface listener and persistent data path), HTTPS
readiness and named volume, without mounting host-loopback development JSON.
The strict example uses the actual identity JSON and existing Compose override.
TOML and a separate public-key-file layout are marked proposed future
targets; neither is represented as the shipped 0.1.0 configuration.

The notes distinguish one-time seed bytes from source-backend metadata/history
import, keep initializer/sync/import/gRPC unavailable and ignored `FAKE_GCS_*`
inputs explicit, and describe Windows/macOS restart versus unverified OS-crash
and power-loss recovery. README publication warnings remain in both languages.
No new comparison claims about third-party products were introduced.

## First useful release and included workflow mapping

The first-useful-release requirements were manually checked against
[the roadmap](../roadmap.md#first-useful-release), the notes' entire Included
workflows section and the ledger. Every positive workflow claim below belongs
to IMPLEMENTED or PARTIAL rows. PARTIAL means only the documented subset has
evidence; it is not promoted by appearing in release notes. The table is an
evidence cross-reference, not a second implementation-status source.

| Notes claim / first-release requirement | Ledger rows and recorded status | Evidence and supported boundary |
| --- | --- | --- |
| JSON multiple-bucket CRUD, prefix and paginated listing | JSON-BUCKETS-001/002/005/006: PARTIAL | [Bucket core](bucket-core-parity.md); scoped JSON methods, not all Google bucket fields |
| Bucket labels and default storage class PATCH/PUT | JSON-BUCKETS-008/013: PARTIAL | [PATCH](bucket-metadata-patch.md), [PUT](bucket-metadata-put.md); explicit local omission/null rules |
| Version history and default event-based holds | BEHAVIOR-OBJECT-029/035 and BEHAVIOR-BUCKET-012: PARTIAL | [Core/history](bucket-core-parity.md); supported generation/hold effects only |
| Bounded media and multipart uploads | JSON-OBJECTS-007, BEHAVIOR-OBJECT-008/009, DESIGN-070: PARTIAL | [Media](object-media.md), [multipart](multipart.md); fixed admission/body limits, no new memory measurement claim |
| Resumable initiation/chunks/status/cancel/finalization | BEHAVIOR-OBJECT-010/011/012/013/014: IMPLEMENTED; JSON-OBJECTS-007: PARTIAL | [Resumable](resumable-upload.md); local session scope and provider gaps remain distinct |
| Object metadata/bytes/list/update/delete and byte ranges | JSON-OBJECTS-004/005/008/010/015: PARTIAL; BEHAVIOR-OBJECT-016: IMPLEMENTED | [Listing/metadata](object-list-metadata.md), [ranges](object-range.md), [media](object-media.md) |
| Copy, compose and multi-call rewrite | JSON-OBJECTS-002: IMPLEMENTED; JSON-OBJECTS-003/012: PARTIAL; BEHAVIOR-OBJECT-023/024: IMPLEMENTED | [Derived objects](derived-objects.md); scoped bytes, continuation and compose contracts |
| Mutation generation/metageneration conditions and checksums | BEHAVIOR-COMMON-011 and BEHAVIOR-OBJECT-005: IMPLEMENTED | [Basic JSON](basic-json-contract.md); supported conditions and checksum containers |
| Permissive/strict local authentication, registered principals and bucket IAM | DESIGN-001/003/014: IMPLEMENTED; DESIGN-002: PARTIAL | [Strict auth](strict-auth.md), [bucket IAM](bucket-iam.md); local test principals, no Google identity issuance |
| Public/private access, distinct public grants and pure URL construction | UX-005, DESIGN-012/013/046: IMPLEMENTED | [Public reads](public-object-reads.md); URL generation alone grants no access |
| XML object GET/HEAD/PUT | EXT-XML-012/017/022: PARTIAL | [Signed access](signed-urls.md), [XML PUT](xml-put-object.md); scoped object endpoints |
| V4 RSA signing/verification and URL journey | DESIGN-030/032/033/034/036/038/045 and UX-006: IMPLEMENTED; DESIGN-035/037/044: PARTIAL | [Signed access](signed-urls.md); SDK endpoint/hash and query/error gaps retained |
| Virtual hosts and two response overrides | DESIGN-048: IMPLEMENTED; DESIGN-035 and EXT-XML-012/017: PARTIAL | [Virtual hosts](xml-virtual-host.md), [overrides](xml-response-overrides.md); opt-in DNS/TLS and local validation scope |
| Public access prevention | BEHAVIOR-BUCKET-014, DESIGN-022/023/025: IMPLEMENTED | [PAP](public-access-prevention.md); parent policy fidelity remains LOCAL_SIMULATION |
| JSON/XML CORS and existing browser journey | DESIGN-051/052/053 and UX-007: IMPLEMENTED | [CORS](cors.md), [signed browser](signed-browser.md); CORS does not grant permissions |
| Persistent SQLite/opaque blobs, restart and atomic publication | DESIGN-063/072/073/074 and UX-008: PARTIAL; DESIGN-075: IMPLEMENTED | [Media/recovery](object-media.md); selected interruption boundaries, no tested power-loss guarantee |
| Fixtures / one-time seed | DESIGN-062: PARTIAL; UX-002: IMPLEMENTED | [Seed](seed.md), [platform/container checks](seed-platform-containers.txt), [Linux checks](seed-platform-linux.txt); seed bytes, not source-backend import |
| HTTP/local HTTPS, CLI/environment/JSON settings and probes | DESIGN-064/065: IMPLEMENTED; DESIGN-067: PARTIAL | [TLS](local-tls.md), [foundation tests](p0-tests.txt); flat JSON, bounded probe and declared readiness scope |
| Source-built non-root Linux containers | DESIGN-058/059/060/066: IMPLEMENTED | [Container evidence](p0-containers.txt), existing Foundation run below; public artifacts remain unverified |
| Go/Ruby SDK subsets, including HTTPS | DESIGN-082/083 and UX-004: PARTIAL | [TLS SDKs](local-tls.md), [signed SDKs](signed-urls.md), local results below; Go runs in CI, Ruby does not |

This covers the roadmap's JSON, public/private access, URL, persistence,
fixtures, Go/Ruby, CORS and local HTTPS requirements at their declared subset
boundaries. It does not close distribution gates: DIST-001/002/003 remain
PARTIAL, source candidates still need pre-tag verification, and the local release
gate is FAIL. Rails, lifecycle, gRPC and full reference parity are not added as
first-release prerequisites.

## Data and documentation consistency

Source inspection confirms the data-directory database is `catalog.sqlite3` and
its final `PRAGMA user_version` is **13** in `src/storage/catalog.rs`. The
resumable schema's version **7** in `src/storage/resumable.rs` is an earlier
migration in that same catalog; startup continues through the version-13 PAP
migration. It is not a second database's current schema. The notes define future
0.x forward-upgrade preservation as an obligation, not an implemented future
migration, and retain backup and downgrade limits.
They now explicitly state that compatible schemas 0–13 are accepted except
unreleased schema 2, whose deleted history cannot be recovered; future schemas,
incompatible DDL, corruption and nonempty unversioned catalogs are rejected.

Configuration-reference inspection used:

```sh
git grep -nE 'config/[a-z]+\.toml|identities\.toml' -- README.md docs
git ls-files config/ ':(glob)compose*.yaml'
```

After correction, the grep produced **no matches**, exit **1**. All matches
before correction were the strict/identity TOML statements in the specification;
they now describe tracked JSON and actual override mounts. Remaining TOML fenced
examples are explicitly proposed future material. The tracked-file inventory
was:

```text
compose.http.yaml
compose.seed.yaml
compose.strict.yaml
compose.tls.yaml
compose.yaml
config/development.json
config/identities.json
```

The Compose/strict examples were compared with these files; the override
mounts the JSON registry and sets strict environment variables, with inline
public keys rather than a separate generated PEM mount. No nonexistent config
file is described as shipped. An inline Node assertion check compared the
Compose example with the image CMD, volume and HTTPS readiness in checked-in
Compose. It does not execute containers; their historical results remain below.
Another inline Node check resolved Markdown file targets and heading anchors in
both READMEs, the specification, notes, contract and evidence report. It checked
relative paths and tag-pinned repository paths against the current tree:

```text
PASS: 102 relative links and 12 tag-pinned links across 6 documents, including anchors
PASS: Compose example preserves image listener/data CMD and matches HTTPS readiness and volume
```

The evidence file introduced by `3c595af` is present in the same delivered change
set as its contract link; the earlier contract-only commit did not include it.

README publication warnings remain preparation/unreleased and link here for
pending gates. The notes now use tag-time statements: archives/checksums attach
to the Release, and anonymous image access is checked after publication.
`dist-post-publication` owns README, container usage, release-policy and companion
warnings, while notes must be finalized **before** `dist-tag-publication`.

## Release body rendering

`binaries.yaml` passes the tagged notes directly to `gh release create` as
`--notes-file`. The notes were rendered through GitHub's context-free Markdown
API without creating a Release, tag, comment or other published content:

```sh
gh api -X POST /markdown --input - < <(
  jq -n --rawfile text docs/en/releases/0.1.0.md '{text:$text, mode:"gfm"}'
)
```

The preparation render exited **0**. HTML assertions confirmed all **12** hrefs
use tag-pinned repository URLs, with zero tables/preformatted blocks matching
the source and no raw table delimiter. The current notes describe tag-time gates
through `binaries.yaml`'s `publish` dependencies (`validate`, `foundation`,
`build`), rather than embedding any tag run ID. Historical Foundation run IDs
remain in the pre-tag evidence below. All repository links are pinned to
`blob/v0.1.0/`; no tag exists yet, so live tag-link HTTP resolution is not
claimed. The final `release-notes-body` case is FAIL because its marker
condition fails: `finalize: source-identity`, `finalize: resource` and
`finalize: schedules` are still present. Before publication, confirm the behavior
baseline and candidate comparison under the publication contract, add measured
idle/1 GiB RSS, separate cgroup memory and CPU seconds plus tag-pinned resource
evidence, and
the actually unverified schedule/candidate-build limits. Rerender and check
the final notes and candidate-tree paths/anchors before the tag operation.

The finalization guard `! rg -n '<!-- finalize:' docs/en/releases/0.1.0.md`
exited **1**, as expected for these unfinished items:

```text
3:<!-- finalize: source-identity -->
135:- <!-- finalize: schedules -->
143:- <!-- finalize: resource -->
```

## Frozen source identity

`release-source-identity` compares protected paths defined in the contract.
Preparation commit `efa27b8db2b5b772fd04849362f500e99cf43679`, containing the
revised notes and contract, was checked against behavior baseline `71883c0`.
Both commands below exited **0** with **empty
output**, including `Justfile`, embedded upstream schemas, scripts and
distribution inputs:

```sh
git diff --name-only 71883c0 efa27b8db2b5b772fd04849362f500e99cf43679 -- \
  src/ tests/ Cargo.toml Cargo.lock Dockerfile rust-toolchain.toml \
  .cargo/ .github/ .dockerignore Justfile distribution/ xtask/ scripts/ \
  ':(glob)compose*.yaml' config/ fixtures/ sdk-tests/ \
  compatibility/upstream/ compatibility/dependency-status.json LICENSE-MIT LICENSE-APACHE
git diff --name-only 900393902a1a7a58ac34d5a77d37861105d106ef 71883c0 -- \
  src/ tests/ Cargo.toml Cargo.lock Dockerfile rust-toolchain.toml \
  .cargo/ .github/ .dockerignore Justfile distribution/ xtask/ scripts/ \
  ':(glob)compose*.yaml' config/ fixtures/ sdk-tests/ \
  compatibility/upstream/ compatibility/dependency-status.json LICENSE-MIT LICENSE-APACHE
```

The historical preparation comparison passes only for that recorded commit.
The eventual release tag points to the
tested notes/evidence-bearing candidate, not necessarily `71883c0`. This result
does not verify a future tag candidate: `dist-tag-publication` must repeat the
same protected-path comparison immediately before publishing that exact SHA.
This slice establishes no dependency-pin exception. `dist-tag-publication` must
define baseline/revision/tag SHA correspondence, permitted differences and their
classification/revalidation, and freshness-window recovery in its contract,
using the concrete pin locations listed in the slice. Those acceptance cases
remain PENDING. Historical PASS only compares the recorded source with
`71883c0`; it does not verify a future pin revision or tag. Parallel configuration
changes are not implicitly part of 0.1.0. The Foundation inspection satisfies the
linked `release-ci-evidence` cases for DESIGN-059 and DESIGN-099; eight-archive
execution remains in `dist-manual-dispatch`.
The preparation case is PASS only for its recorded SHA; the separate
`release-tag-source-identity` case is PENDING and owned by `dist-tag-publication`.
The tag's own execution results belong in the main-branch ledger after
publication, avoiding a commit containing its own future SHA/run ID.

### Protected inputs after main rebase

The branch rebased without conflicts onto main
`280a84c6696c786d6f0deb4e82d5884aa18345b2`, containing `repo-lints`,
`config-contract` and `import-contract`. Rebased preparation commit
`598f222f58fa4906c67d23a7fadba47be6c81050` contains the corrected notes and
publication handoff. Repeating the full protected-path comparison against
`71883c0` exited **0 with nonempty output**, so equality is **not** established:

```sh
git diff --name-only 71883c0 598f222f58fa4906c67d23a7fadba47be6c81050 -- \
  src/ tests/ Cargo.toml Cargo.lock Dockerfile rust-toolchain.toml \
  .cargo/ .github/ .dockerignore Justfile distribution/ xtask/ scripts/ \
  ':(glob)compose*.yaml' config/ fixtures/ sdk-tests/ \
  compatibility/upstream/ compatibility/dependency-status.json LICENSE-MIT LICENSE-APACHE
```

```text
Cargo.toml
compatibility/upstream/source-storage-contract.json
scripts/compare-reference-emulator.sh
scripts/verify-source-storage-contract.sh
src/storage/catalog.rs
src/storage/objects.rs
tests/catalog.rs
tests/common/mod.rs
xtask/Cargo.toml
```

`repo-lints` commit `2307997` contributes the Cargo manifests, storage sources
and tests above. `config-contract` commit `626ab91` contributes the reference
comparison script; `import-contract` commit `280a84c` adds the source-storage
contract JSON and verifier script. The same comparison from main to the rebased
preparation commit has empty output: this slice adds no protected-input changes
to main.
Historical Foundation evidence at `9003939` does not validate the rebased tree.
`dist-tag-publication` must confirm which commit defines the 0.1.0 behavior
baseline and how to compare/revalidate the exact candidate after this integration;
this slice does not decide that question or approve these differences.

The `.dockerignore` inclusions, Dockerfile COPY sources and Rust embedded-file
macros were inspected. An inline Node assertion check resolved each literal
`include_str!`/`include_bytes!` path relative to its Rust file and checked it and
the included tracked Docker-context files against the protected set:

```text
PASS: 15 embedded-file references and 10 Docker context inclusions covered by protected paths
```

## Existing remote evidence

This table records **pre-tag state**. Final tag gate results are recorded by
`dist-post-publication` in the main-branch ledger, not in this frozen snapshot.

Read-only commands were run; no workflow was dispatched:

```sh
gh run list --workflow ci.yaml --limit 3 \
  --json databaseId,headSha,conclusion,event,url
gh run view 39995036204 --json headSha,conclusion,jobs,url
gh run view 39995036204 --log
git diff 900393902a1a7a58ac34d5a77d37861105d106ef 71883c0 -- \
  Dockerfile Cargo.toml Cargo.lock src scripts .github/workflows \
  rust-toolchain.toml sdk-tests xtask
gh run list --workflow binaries.yaml --limit 3 \
  --json databaseId,headSha,conclusion,event,url
gh release list --limit 3 --json tagName,isDraft,isPrerelease
gh run list --workflow dependency-check.yaml --limit 5 \
  --json databaseId,headSha,conclusion,event,url
gh run view 39925933967 --json createdAt,conclusion,event,jobs,headSha,url
```

All commands exited **0**. The source-input comparison produced no diff.
[Foundation run 39995036204](https://github.com/shiroemons/gcs-mimic/actions/runs/39995036204)
has `conclusion: success` at the exact source SHA above. Completed jobs:

| Job | Job ID | Result |
| --- | --- | --- |
| `rust (pinned)` | 120556231082 | success |
| `rust (stable)` | 120556230684 | success |
| `containers (ubuntu-24.04, linux/amd64)` | 120556231099 | success |
| `containers (ubuntu-24.04-arm, linux/arm64)` | 120556231089 | success |
| `sdk-go` | 120556231120 | success |

The container logs identify the native Ubuntu runner images and execute
`bash scripts/smoke-containers.sh` for their respective CPU. The source includes
the notice-augmented Dockerfile. This closes the existing native AMD64 and remote
Foundation execution cases for DESIGN-059 and DESIGN-099. It does not verify
eight native archives, a new publication image, or future source changes.
No hosted run of this documentation commit is claimed.

The binary workflow list and GitHub Release list were empty. No public GHCR
publication is established. DIST-001/002/003 retain their pending platform,
manual execution, immutable-publication and anonymous-access cases.

[Dependency run 39925933967](https://github.com/shiroemons/gcs-mimic/actions/runs/39925933967)
was scheduled on **2026-10-23**, a Friday, at source
`d81f4190429460876486a33fc0119d78a1e0191b`; its freshness job succeeded.
The list also returned successful scheduled run `39749606987`. This corrects
the old claim that no scheduled run was observed, while leaving weekend
execution pending. DEP-009 records executed pinned/current-stable/native lanes;
latest-dependency candidate builds remain unimplemented. DEP-006 still lacks an
executed release and weekly exception-review cycle.

## Local verification

The recorded `just check-dependencies` result remains exit **1**, with a
distroless digest update and all other checked inputs CURRENT at that check.
The single dependency-output artifact below preserves it. Freshness,
`just pre-push`, SDK, container and guide journeys were **not rerun** for the
publication-contract wording changes; their executed results and the failed
full invocation remain recorded here.

Historical local Rust/SDK/container results below predate the main rebase.
They are not verification of the newly integrated protected inputs. The rebase
comparison above replaces the earlier empty working-tree comparison. The
publication-contract corrections affect DIST-002, the DEP-006 refresh owner,
historical-source qualifications in DESIGN-059/099 and DEP-009, and
release-verification delivery conditions; implementation statuses do not advance.

### Verification after main rebase

An initial `just check` after integration of main `626ab91` exited **101** inside
the Codex sandbox. Its recipe-order self-checks, fmt and clippy
passed, followed by 157 passing library tests. All 93 `auth_http` tests then
failed because the child HTTP servers could not start in that sandbox:

```text
error: Operation not permitted (os error 1)
ready event: Error("EOF while parsing a value", line: 1, column: 0)
test result: FAILED. 0 passed; 93 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.85s
error: test failed, to rerun pass `-p gcs-mimic --test auth_http`
error: recipe `check` failed on line 9 with exit code 101
```

Supplemental Rust verification used
`cargo test --workspace --locked --offline` outside the sandbox, with identical
source: **665 passed, 0 failed, 0 ignored**, exit **0**, confirming the initial
failure depended on the sandbox. These results do not turn the failed invocation
or the full release-local gate into PASS.

After rebase onto main `280a84c`, one complete `just check` invocation outside
the sandbox exited **0** on the final protected inputs: **665 passed, 0 failed,
0 ignored**. It passed recipe-order/failure-propagation self-checks, fmt, clippy,
Rust tests, generated-document checks, the network-free container-publication
self-check and shell syntax/help/platform rejection checks. It did not build
containers or execute SDK suites. No complete `just pre-push` or freshness
check was rerun; `release-local-gates` remains FAIL.

The post-rebase documentation checks passed:

- `cargo xtask compatibility validate`, `render` and `check`: 591 rows.
- Repeated generation: both matrices unchanged byte for byte.
- Markdown lint: six authored documents and both generated views, zero issues.
- Link/anchor checks: 102 relative and 12 tag-pinned links across six documents.
- Context-free GitHub Markdown render: 12 tag-pinned hrefs, matching source/HTML
  table and code-fence counts, no raw table delimiter.
- Assertions: unowned main ledger rows and delivery conditions preserved,
  complete pin inventory in contract/ledger, workflow descriptions match source,
  three finalize markers, publication contracts PENDING, release-local/body
  gates FAIL and DESIGN-099 PARTIAL.

### Historical pre-tag execution

`just check-dependencies` exited **1**. The official-source checker found a
runtime digest update; all other checked inputs reported CURRENT. The
[complete output](dist-release-notes-dependencies.txt) preserves the failure.

```text
docker distroless/cc-debian13:nonroot: UPDATE_AVAILABLE current=sha256:83b9b50586253821cf63ffcec87d3d94265a093ad30da232039af962dd8f4edf latest=sha256:3ee00291cf386ec52a552e6a858d555debd74776eb4bfa53fa3df2cd6677515e
error: recipe `check-dependencies` failed on line 13 with exit code 1
```

This is upstream drift, not resource contention or a network retrieval error.
The docs-only slice does not change Docker inputs. `dist-tag-publication` owns
adopting and revalidating the update before publication; no exception is approved.

Contract Markdown lint and `cargo xtask compatibility validate` passed before
the contract commit; validation covered 591 feature rows. Final Markdown,
generated-view and local gate outcomes follow.

| Check | Result |
| --- | --- |
| Markdown lint: six authored/changed documents | PASS, 0 issues |
| Markdown lint: both regenerated feature matrices | PASS, 0 issues |
| `cargo xtask compatibility validate`, `render`, `check` | PASS, 591 rows |
| `git diff --check` | PASS |
| `just pre-push`, invoked once | FAIL, exit 18: Ruby environment selection |
| Rust, fmt, clippy, shell and docs | PASS; 665 tests, 0 failed/ignored |
| Go SDK stage of that invocation | PASS |
| `just check-sdk` with matching pinned Ruby/gems | PASS, exit 0 |
| `just check-containers`, one execution for both architectures | PASS, exit 0 |

Authored documents used default Markdown rules and their file-local settings.
For the generated matrices, MD013 was disabled for existing long generated
paragraphs/tables and MD024 used `siblings_only: true` because categories repeat
under different feature kinds. The evidence report's long digest excerpt and
ledger/evidence mapping table use file-local MD013 code-block/table exceptions.
No other lint rules were disabled.

The initial `pre-push` invocation selected Ruby 3.4.1 instead of the pinned 4.0.9,
so its container stage was not reached. Relevant actual failure output:

```text
Your Ruby version is 3.4.1, but your Gemfile specified 4.0.9
error: recipe `check-sdk` failed on line 17 with exit code 18
```

Selecting the separate mise 4.0.9 installation first found missing gems;
selecting the existing shared gems then exposed a native-extension Ruby linkage
mismatch. These were local environment failures, not source regressions. The
matching existing Ruby installation and gem home passed `bundle check` and the
origin self-check, then the complete SDK recipe passed without source, lockfile
or installed-package changes. The failed full invocation was not rerun or
reported as passing; the SDK and previously unreached container stages were
completed separately. The equivalent Rust/SDK/container stages all passed.

The final SDK environment selected Ruby 4.0.9, Bundler 4.0.23 and the committed
gem graph. With `PINNED_RUBY_BIN` and `PINNED_GEM_HOME` identifying those existing
local installations, the portable command form is:

```sh
env PATH="$PINNED_RUBY_BIN:$PINNED_GEM_HOME/bin:$PATH" \
  GEM_HOME="$PINNED_GEM_HOME" GEM_PATH="$PINNED_GEM_HOME" just check-sdk
just check-containers
```

The SDK output confirms complete Go tests, Ruby origin self-check,
permissive/strict HTTP six-bucket/nine-object-method journeys, derived byte
checks, three resumable chunks, signed GET/PUT and response overrides. Existing
HTTPS/browser evidence was reviewed; this slice did not rerun those journeys.

Both images built on Docker Desktop: ARM64 native and AMD64 emulated. Both
passed probes, host/peer networking, bucket/object CRUD, stop/start and recreate
persistence, intentional disposable-volume reset, non-root/read-only/resource
settings, SIGINT/SIGTERM and seed/no-replay checks. No resource-contention
failure occurred. These remain local test images, not published artifacts.

`release-local-gates` is **FAIL**, and release is blocked. PASS requires
all-CURRENT freshness/exit 0, zero changed-Markdown lint issues, final-source
`just pre-push` exit 0 and a successful regenerated compatibility check.
Freshness exited 1 and the full `pre-push` invocation exited 18; separate passing
stages do not satisfy that gate. No expiring exception satisfies freshness.
The former documentation-audit PASS for this test ID was incorrect and has been
removed from the ledger. Notes, consistency and source-identity cases are
separate from this failed gate.
Those three cases pass on the recorded workflow/schema review, tracked-file and
link checks, and protected-input comparison. This does not authorize release.
DESIGN-059 retains IMPLEMENTED from existing remote execution evidence.
DESIGN-099 is PARTIAL: its local-checks case is FAIL, and the latest 2026-10-24
full pre-push failure is recorded under `local-pre-push-suite` with an explicit
Ruby-selection deviation. A separately scoped improvement should select the
pinned Ruby instead of relying on PATH. DIST-001/002/003 and DEP-005/006/009
remain PARTIAL. No acceptance case
for actual public distribution, all-target release execution, weekend schedules
or latest-dependency candidates is marked complete.

## Remaining gates

No tag push, workflow dispatch, GitHub Release, GHCR push or visibility change
was performed. This slice changed no dependencies or runtime code; the main
rebase brought in the protected-input differences recorded above. This slice
does not
measure performance, run the eight-target release workflow, validate weekend
schedules or create a latest-dependency candidate lane. Resource measurement,
native archives and publication remain later separately scoped work.

## Final tag candidate

Pre-tag state recorded on 2026-10-26 for the Linux-only 0.1.0 release. The
earlier sections remain historical preparation evidence.

The behavior baseline is `77a5da3`. The tag candidate adds only release notes
and documentation on top of it. Re-run immediately before tagging, with
`<candidate>` replaced by the exact commit to tag; empty output is required:

```sh
git diff --name-only 77a5da3 <candidate> -- \
  src/ tests/ Cargo.toml Cargo.lock Dockerfile rust-toolchain.toml \
  .cargo/ .github/ .dockerignore Justfile distribution/ xtask/ scripts/ \
  ':(glob)compose*.yaml' config/ fixtures/ sdk-tests/ \
  compatibility/upstream/ compatibility/dependency-status.json LICENSE-MIT LICENSE-APACHE
```

The first manual **Release binaries** dispatch on the previous main
(`cc5df0e`, run `40209212101`) failed: Windows did not compile because
Unix-only APIs lacked `cfg` gates, musl archives failed to link `renameat2`,
and macOS deadline tests failed on the runner. `954137d` and `8050452` fixed
those failures. A second dispatch (`86d6bf2`, run `40211855017`) passed every
Linux target and container check but still failed macOS TLS tests and Windows
catalog tests, ending with a Windows test process abort. `5908b3c` therefore
removed macOS and Windows from the release matrix for 0.1.0.

The third dispatch (`84277b2`, run `40252492903`) succeeded: `validate`
including dependency freshness, Foundation pinned/stable Rust, both Linux
container smoke checks, the Go SDK suite and the four Linux GNU/musl
build/test/package jobs. `publish` and `publish-container` were skipped as
expected for a manual dispatch. The only later protected-input change,
`fa7c3bb`, renames the Foundation input that skips the `portability` job in
release workflows, because the previous condition also skipped it on pushes;
release jobs and steps are otherwise unchanged.

A first `v0.1.0` tag on `a00b950` (run `40254317371`) passed `validate`,
Foundation and all four Linux builds, then failed in `publish` before creating
anything: `gh release create` received the unmatched `dist/*.zip` glob
literally. No GitHub Release, asset or container image existed. With the
owner's approval that tag was deleted, `77a5da3` collects the existing assets
with `nullglob`, and `v0.1.0` was recreated on the new candidate.

At this state:

- `just check` passed on `86d6bf2` with the new cross-target Clippy step. The
  later protected changes touch workflows and documentation only.
- `cargo xtask dependencies check` reported every input CURRENT, including the
  distroless runtime digest that blocked earlier preparation.
- `docs/en/releases/0.1.0.md` contains no finalization markers; every
  tag-pinned link targets an existing path and heading in the candidate tree.
- Not run: `just pre-push` (local Ruby SDK and both container architectures),
  a Linux re-run of the intermittent truncated copy download seen once in
  run `40209212101` (60 local Linux container runs and 20 macOS runs passed),
  and anonymous pulls, which belong to post-publication checks.

## Publication results

Recorded on main after publication, 2026-10-26.

- **v0.1.0** (tag on `d29b4e7`, run `40255031827`): the GitHub Release with
  four Linux archives and `SHA256SUMS` was created. One `aarch64` musl test
  hit its 50 ms catalog budget on the first attempt and passed on rerun. The
  container manifest job stopped twice: first because the new package was
  private, then because `scripts/container-release.sh` accepted only SLSA v0.2
  BuildKit provenance while BuildKit 0.34 records SLSA v1. Reruns use the
  tag's script, so **0.1.0 has no container image**.
- **v0.1.1** (tag on `abbe002`, run `40259191681`): every job passed on the
  first attempt, including both native platform images, SBOM/provenance
  checks, the multi-architecture index and the digest asset. Tags `0.1.1`,
  `0.1` and `latest` resolve to
  `sha256:6d2dafd0ce5e0c6ff2bceed8e2f2b0f50e8b8ae60e0d538cca3e2ec9689c8db4`.
- Post-publication checks on a macOS ARM64 Docker Desktop host: anonymous
  `docker pull` of `linux/amd64` and `linux/arm64` succeeded and the image
  reports `gcs-mimic 0.1.1`. The README quick start and the fake-gcs-server
  Compose replacement walkthrough both pass unchanged against the published
  image.
- **v0.1.2** (tag on `dd2f2d1`, run `40267644906`): adds both macOS archives.
  The dry-run dispatch `40265461685` first passed all six builds and listed the
  seven Release assets without publishing. On the tag, the x86-64 macOS job
  failed two tests when the 5 s client read timeout expired under load; one
  rerun passed, then publish and the container jobs succeeded. `0.1.2`, `0.1`
  and `latest` resolve to
  `sha256:567c27bc790b0951c921171921cf509fd4cfc7f770c1fa86a7eccdf32c76e69a`;
  anonymous pulls of both platforms report `gcs-mimic 0.1.2`. The downloaded
  `aarch64-apple-darwin` archive matched `SHA256SUMS` and reported its version.
  Main later raised the client hang guard to 30 s.
- Still not run: `just pre-push`, an identical-input manifest retry and
  native Windows archives.
