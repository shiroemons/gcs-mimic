# Initial release notes contract

The selected release is **0.1.0**, with preparation behavior baseline `71883c0`.
The eventual tag must point to a tested candidate containing release notes and
distribution evidence; it is not expected to point directly to the baseline.
Configuration compatibility changes, bucket initialization files, directory
synchronization and source-state import belong to later 0.x releases.
This slice changes documentation and ledger evidence only.

This contract records **pre-tag preparation state**. Present-tense pending and
blocking descriptions refer to that state, not the final tag's gate results.
Before tagging, `dist-tag-publication` must update preparation claims that no
longer apply and require pre-tag gates to pass; tag execution results are recorded
by `dist-post-publication` in the main-branch ledger.

## Acceptance cases

- `release-source-identity` (DIST-002): compare the frozen behavior baseline with
  the recorded preparation commit over the protected paths below. PASS requires
  empty output; record the exact SHA and output. This checks preparation only.
  Its historical PASS predates the main rebase and does not verify the rebased
  candidate; the nonempty comparison is recorded in the evidence below.
- `release-tag-source-identity` (DIST-002): repeat the comparison immediately
  before `dist-tag-publication` for the exact tested tag candidate, including
  final notes/evidence. This remains PENDING under `dist-tag-publication`, whose
  contract must define dependency-pin revision identity and permitted differences
  before applying them. This slice authorizes no protected-input change.
  Confirm the 0.1.0 behavior baseline after integration of main's protected-input
  changes; this slice does not select that baseline or approve those differences.
- `release-notes-body` (DIST-002): render the notes through GitHub's Markdown API
  without repository context, as a Release body. Require no false publication
  statements and no broken links: use absolute repository URLs pinned to
  `v0.1.0`, check their paths/anchors against the candidate tree and inspect the
  rendered hrefs. The not-yet-created tag cannot be tested by HTTP before tagging;
  repeat the render and target checks on final notes before publication. FAIL if
  any `<!-- finalize: ... -->` marker remains. Replace the resource marker with
  measured idle/1 GiB transfer RSS, cgroup memory and CPU seconds from
  `dist-resource-measurement`, with a tag-pinned evidence link. Replace the
  schedule marker with the actual unverified weekend execution, weekly exception
  review and latest-dependency candidate status at tagging. Describe tag-time
  platform gates through workflow dependencies, without the tag SHA's run ID.
  Replace the source-identity marker with the confirmed behavior baseline and
  verified candidate comparison under `dist-tag-publication`'s contract.
- `release-notes-scope`: describe delivered workflows, compatibility impact,
  verified platforms and known limitations. Map every included-workflow claim
  and every first-useful-release roadmap requirement to ledger IDs, their actual
  IMPLEMENTED/PARTIAL status and existing evidence in a table. Include local-only
  Ruby SDK evidence, ignored `FAKE_GCS_*`, absent initializer/sync/source import
  and gRPC, and Windows/macOS recovery limits. Distinguish seed bytes from source
  import. State the current data-directory schema, separate application/schema
  versions, the forward-upgrade obligation for later 0.x releases, backups and
  unguaranteed downgrade. Describe accepted schemas 0–13, rejection of unreleased
  schema 2 and future schemas, and existing integrity/schema checks. Migration
  means no earlier released gcs-mimic version;
  fake-gcs-server users follow the migration guide, not an automatic conversion.
- `release-doc-consistency`: keep both README summaries and publication warnings
  consistent with the notes and ledger. Files described as shipped must exist
  in `git ls-files`; check every configuration TOML reference using the grep
  below and allow TOML only in explicitly proposed/future material. Verify
  relative link targets in changed documents, including the evidence link.
  Examples referencing real files must work in combination: compare Compose
  command overrides, listener, persistent data path, scheme and readiness probe
  with the actual configuration and Dockerfile; use the checked-in Compose layout.
- `release-ci-evidence` (DESIGN-059/099): inspect existing Foundation execution
  and compare its protected source inputs only with baseline `71883c0`. PASS
  requires successful pinned/stable and native container jobs at recorded SHA
  `900393902a1a7a58ac34d5a77d37861105d106ef` and empty protected-input diff
  against that baseline. This historical PASS does not verify any later pin
  revision or tag candidate. Record native CPU execution separately from local
  AMD64 emulation; do not infer native archives, publication or a candidate lane.
- `release-pin-revision-contract` (DIST-002): PENDING, owned by
  `dist-tag-publication`. Define the relationship between baseline, revalidated
  pin-revision and tag SHAs, protected-input comparisons, and where final gate
  results are recorded without a commit referring to its own future SHA/run ID.
  Own the required pin refresh, including the known newer distroless digest, and
  its revalidation before publication.
- `release-pin-diff-contract` (DIST-002): PENDING, owned by
  `dist-tag-publication`. Define permitted dependency-update differences and their
  classification/revalidation procedure using the concrete pin locations below.
- `release-tag-freshness-contract` (DIST-002): PENDING, owned by
  `dist-tag-publication`. Resolve the pre-push/tag-validation freshness window
  and owner-authorized recovery when freshness fails after tag push but before
  any Release/assets exist; do not assume tag deletion or a patch-version bump.
- `release-local-gates` (DIST-002): PASS requires `just check-dependencies`
  exit 0
  with every input CURRENT, changed Markdown lint with zero issues, final-source
  `just pre-push` exit 0 and `cargo xtask compatibility check` exit 0 after view
  regeneration. Any failed condition is FAIL and blocks release. Expiring
  incompatibility exceptions must remain explicit deviations with failure
  evidence and a review deadline; they do not satisfy freshness or turn this
  case PASS. Recording a failure or passing stages separately is not a gate pass.

DIST-002 owns the source-identity, notes, consistency and local-gate cases.
DESIGN-059 and DESIGN-099 each link the read-only CI case in their ledger
acceptance cases, test IDs and verification. DIST-001/003 and DEP-005/006/009
retain contextual notes and their existing scope; the weekday DEP-005 and
remote-lane DEP-009 cases remain separately recorded. Existing pending cases
remain in force. The delivery plan has no independent implementation status.

## Protected source inputs and checks

The protected path set covers application/test/build inputs, toolchains,
distribution and verification gates, SDK graphs, shipped fixtures/configuration
and license material. `Justfile` is included explicitly: it changed in `9003939`
and must not be omitted from comparisons. Compare against `71883c0`, and compare
the historical Foundation SHA against that baseline with the same path set:

```sh
git diff --name-only 71883c0 HEAD -- \
  src/ tests/ Cargo.toml Cargo.lock Dockerfile rust-toolchain.toml \
  .cargo/ .github/ .dockerignore Justfile distribution/ xtask/ scripts/ \
  ':(glob)compose*.yaml' config/ fixtures/ sdk-tests/ \
  compatibility/upstream/ compatibility/dependency-status.json LICENSE-MIT LICENSE-APACHE
git grep -nE 'config/[a-z]+\.toml|identities\.toml' -- README.md docs
git ls-files config/ ':(glob)compose*.yaml'
```

An exit-zero diff alone does not mean equality: its output must be empty.
Grep exit 1 means no matching TOML paths; otherwise review every match in context.
At publication replace HEAD with the exact candidate SHA and pin that checked
SHA as the tag target under the pending publication contract. Documentation-only
follow-ups may differ from the behavior baseline; protected-input revisions need
the contract and revalidation defined by `dist-tag-publication`.
`compatibility/upstream/` includes the JSON schema embedded by
`src/json/fields.rs`, which changes field-validation behavior.

Audit the set whenever build inputs change: enumerate `.dockerignore` inclusion
rules and `Dockerfile` COPY sources, then resolve each `include_str!` and
`include_bytes!` literal relative to its Rust source file (including multiline
macros). Every resulting tracked file must fall under a protected path. These
inventory commands locate the inputs; they do not replace resolving each path:

```sh
rg -n '^!' .dockerignore
rg -n '^COPY ' Dockerfile
rg -n -U 'include_(str|bytes)!\(\s*"[^"]+"' src/ tests/ xtask/
git ls-files compatibility/upstream/ src/tls/ fixtures/ distribution/
```

## Publication contract inputs and final notes

The decision to release 0.1.0 from the current main behavior remains unchanged.
`71883c0` identifies the preparation baseline. This documentation slice does not
define a dependency-pin exception or a release procedure. Owner-authorized
`dist-tag-publication` owns the actual pin refresh, including the newer distroless
digest, its revalidation, and settlement of these concrete inputs
in the publication contract:

- Baseline, pin-revision and tag SHA correspondence, protected-input comparisons,
  and evidence placement: committing a candidate's own SHA/run ID changes its
  SHA, so it cannot contain its own final execution record.
- The final 0.1.0 behavior baseline after main rebase. Comparison against the
  preparation baseline now includes `repo-lints` changes in `Cargo.toml`,
  `src/`, `tests/` and `xtask/`, plus `config-contract`'s reference comparison
  script and `import-contract`'s source-storage JSON/verifier.
  See the [exact output](../evidence/dist-release-notes.md#protected-inputs-after-main-rebase).
  These are not approved dependency-pin exceptions; this slice does not decide
  which commit becomes the release's behavior baseline.
- Permitted diff kinds and their classification/revalidation procedure, covering
  `rust-toolchain.toml`'s channel; `Cargo.toml` version requirements;
  `xtask/src/release.rs`'s hard-coded rustc version/tests, `MUSL_SOURCE_SHA256`,
  musl source URL and `musl-1.2.5.tar.gz` references; Dockerfile `FROM` tags/digests
  and `# syntax=` frontend digest; workflow actions' SHA pins/comments and
  `.github/workflows/binaries.yaml`'s inline musl source URL/SHA-256;
  `sdk-tests/go/go.mod`'s `go`/`toolchain` and dependency requirements;
  `sdk-tests/ruby/.ruby-version` and `sdk-tests/ruby/Gemfile`; upstream manifest
  release URLs/digests; lockfiles; dependency-status and notice files; and
  version-scoped notice review strings such as `crc32c 0.6.8 only`. These cover
  the Rust/Go/Ruby scope in `compatibility/dependency-status.json` and known
  release pins; they are decision inputs, not an allowlist established here.
- The freshness window between the last pre-push check and network validation
  on tag push. If validation fails before Release/assets exist, the owner must
  decide whether that tag can be removed or a new version is needed. This slice
  grants neither operation and does not change the immutable-release policy.

Historical evidence only verifies the recorded SHA against `71883c0`; it does
not verify revised pins. The known newer distroless digest and failed local gate
still block publication. This slice updates no dependencies or workflow logic.

Proposed `dist-tag-publication` follow-up: enforce the finalize-marker guard and
the protected-input comparison in the tag workflow. Its current `validate` job
checks the version and a nonempty notes file, not markers or links; the manual
guard in the evidence remains necessary until workflow enforcement is implemented.

Before the separately authorized tag operation, finalize the notes after
resource measurement and native archive verification, under that publication
contract. Remove every finalization marker: include measured RSS, cgroup memory
and CPU seconds with tag-pinned evidence, state the actually unverified
schedule/candidate-build limits, and finalize source identity. Require local
gates, release body and the exact candidate's source identity to pass before
tagging; a recorded FAIL blocks publication. Rerender the exact notes consumed by
`binaries.yaml --notes-file` and confirm all statements and tag-pinned
paths/anchors. Describe final tag gates through the workflow's structure;
pre-tag run IDs belong in preparation evidence. Label that tag-pinned evidence
as pre-tag state. `dist-post-publication` records the tag's actual run IDs and
final gate results in the main-branch ledger; the tag's own ledger is a frozen
preparation snapshot. Release-body wording must be final before publication.

## Boundaries and follow-up

No runtime, dependency, workflow or shared-state changes are required. This
slice owns no common runtime component. No design requirement or owner decision
is changed; the publication-contract questions above belong to the later slice.

Manual workflow dispatch, tag push, GitHub Release creation, GHCR publication
and package visibility changes require separate owner authorization and are
excluded. Container resource measurements belong to `dist-resource-measurement`;
this slice makes no performance claim. Read-only inspection of existing
Foundation runs is included, but `dist-manual-dispatch` still owns executing and
checking all eight native archives, archive checksums, Windows DLL imports and
GNU Linux glibc symbols. A Foundation container run does not complete those gates.

The final notes must use only statements true at tag publication, with absolute
tag-pinned links; finalization markers currently block `release-notes-body`.
Preparation state and failed gates live in README and the evidence.
`dist-post-publication` owns switching README, container usage, Japanese
companions and release-policy warnings after anonymous access is verified.
The [evidence](../evidence/dist-release-notes.md) records preparation results
and remaining gates separately. Creating notes does not authorize publication.
