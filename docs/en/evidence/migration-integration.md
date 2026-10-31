# Migration integration scope and verification

<!-- markdownlint-configure-file {"MD013": false} -->

This records the source-checkout migration work against fake-gcs-server v1.56.1,
revision `896ece51e49eed0946089f3089f6343855c0d106`. It is separate from the
published 0.7.0 image and the historical five-SDK scorecard. Implementation
status belongs only to [the feature ledger](../../../compatibility/features.json).
Source inspection, compilation and a permissive score do not establish complete
replacement, strict authentication or consumer application compatibility.

Results below name their source checkpoint. The combined 1674-test, SDK,
browser and image results belong to the earlier integrated source, before the
2026-10-31 XML bucket and operator-control additions. The new scoped checkpoint
below does not establish that the changed source has passed those full gates.

## Latest final integration checkpoint: 2026-10-31

This checkpoint covers the frozen implementation of the approved migration
work. The general Google API inventory is a separate reference, not this
request's task count. Earlier native, SDK, image and browser results below stay
historical and are not substituted for this run. Repeated cases are not added
to unique test totals.

The checkout is based on commit
`e5e38f00e89aa3f33f86553d5dde6402a442c7fa` with uncommitted implementation
changes. The coordinator's sorted runtime-input manifest covers 736 files,
excluding documentation and ledgers, with SHA-256
`0fec76af2b35b3eb4e3dc497cf9c62b0d1ef1f970ec4ab421ddf84186e9ab0dc`.
This manifest identifies the first attempted final run. The authorized repairs
described below changed its inputs, so its results cannot establish passing
gates for the repaired checkpoint.
After repairs, the coordinator froze and verified a new manifest covering 794
inputs, SHA-256
`d49453b0dd8ee50d39404dea19b880274f488f560a634bcd4c0a5d7c45154a2b`.
That run completed with 62 of 64 test binaries passing, two failing, 966
library tests passing and four doctests passing. Its failures and the subsequent
fixture-only repairs are recorded below. The first native-passing manifest uses the
same 794 paths and has SHA-256
`95d827c87a8ca7f44b3dd9ad0f7ac670182384d86852e3c327f0fd23b4b919ab`.
Only the autoclass HTTP and synchronization CLI authority fixtures changed
between those two manifests. The static lane's initial checksum verification
reported 794 matching files and zero nonmatching entries.
The later SDK failures required five authorized source/test changes. The current
candidate uses the same path set and has SHA-256
`1bf0246472c4a6dd99747b7c8c6d022640e552c8bba2355067408d4867935515`.
Its repeated native and cross-target gates passed as recorded below; earlier
passing gates were not substituted for this candidate.
`Cargo.lock` has SHA-256
`a60d1d0c930c0cbec38245578459cf74aa0969db8c1f60cce18943c367aa1515`.
The native host is macOS 27.0.1 on ARM64, using rustc 1.99.1
(`58cf8f9c35b355199727627d07ae34a4e2846d67`, LLVM 23.1.1) and Cargo 1.99.1.

The initial all-workspace/all-target Clippy run exited 101 for one
`clippy::useless_format` finding in a legacy-signature test. The initial
formatting check exited 1 for three files: the ACL snapshot tests, main and
startup authentication. A test expression was changed to `kind.to_string()`
and formatting was corrected in those files. The coordinator reports the
subsequent Clippy run with warnings denied passed in 17.85 seconds, followed
by a passing whole-workspace formatting check. These repairs do not imply
that the remaining final gates passed.

| Gate | Current evidence |
| --- | --- |
| Native Rust workspace tests | Current accepted manifest (`1bf024…`): all 64 test binaries and doctests passed, 1809 passed, zero failed, one explicitly opted-in local Pub/Sub emulator case ignored. The earlier 1808-case checkpoint remains historical; counts from earlier attempts are not added. |
| Full-workspace Clippy and formatting | Passed on the current accepted manifest; warnings denied, all targets |
| Production lint | Passed on the current accepted manifest: zero violations and one existing reasoned suppression |
| Cross-target checks | Passed on the current accepted manifest: Windows MSVC, GNU/Linux and musl Linux target Clippy and production lint, zero violations. These are cross checks, not runtime execution on those hosts. |
| Official HTTPS SDKs, extra native clients and strict Rails | Passed on the current accepted manifest: the five-SDK HTTPS command, Java/.NET/Rust/Arrow client checks, Go facade/export race checks and strict Rails scenarios. The earlier Go 86/3 failure remains below. Exact counts, skipped case and profile limitations are separate below. |
| Linux images, smoke tests, trusted browser and Testcontainers | Both current images built successfully and each passed five prebuilt smoke cases. The same ARM64 image passed three generic Testcontainers scenarios and six trusted Chromium executions containing 149 passing assertions, zero failures. Current identities and limits are below; earlier image/browser results remain historical. |
| Documentation and static script checks | Repeated on the current accepted manifest: documentation/canonical generation, path inspection, owned-document Markdown and diff checks passed. The earlier source-independent script/mock results remain dated facts; generated-matrix default diagnostics remain explicit below. |
| Native package and license collection | Repeated for the current native debug binary: 147 registry packages, 274 indexed nonempty notice files, five selected license byte comparisons and binary-copy comparison passed. This does not verify binary origin or ABI. Linux-only release notices were not run on macOS. |
| Dependency freshness | Failed with exit 1: the existing Composer riscv64 publication-lag exception remains open, review due 2026-11-06. It is not an all-current result. No manifests or lockfiles were updated. |

On the repaired 794-input checkpoint, the coordinator's full-workspace,
all-target Clippy command with `--locked --offline -- -D warnings` passed in
18.49 seconds. `cargo fmt --all --check` passed, and `cargo xtask lint`
passed with zero production violations and one existing reasoned suppression
(`text::stringified_error`); its Cargo development check took 11.66 seconds.
The repeated full Rust command was
`env -u RUSTUP_TOOLCHAIN CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=2
GCSM_TEST_JOBS=2 bash scripts/with-test-tmpdir.sh bash scripts/test-rust.sh`.
It first exposed the two fixture failures below. After their repair, the same
command passed against the first native-passing manifest. Cross-target, SDK, image,
browser and static results are not inferred from the native gates.
The first native-passing log independently totals 1804 passing binary cases and four
passing doctests across three doctest groups, with no failed cases or error
lines. Its Clippy and lint development checks took 0.82 and 11.67 seconds
respectively; these cached check durations are not performance comparisons.
After the SDK repairs, the current accepted manifest passed a repeated full
native run with 1805 binary cases and four doctests: 1809 passed, zero failed
and the same one opted-in Pub/Sub case ignored. Its full Clippy check passed
in 16.77 seconds; formatting and production lint also passed, with zero
violations and the same one reasoned suppression. The coordinator's Windows
MSVC, GNU/Linux and musl Linux cross-target Clippy/lint checks passed with zero
violations and verified no changed inputs at completion. The static lane's new
initial verification also found 794 matching inputs and zero nonmatching
entries. These are actual new-candidate results, not rerun totals added to the
earlier checkpoint.

The autoclass fixture compared timestamp strings lexically even though the
documented representation omits trailing zeros beyond three fractional digits.
Padding the fraction to nine digits before comparing preserves chronological
ordering without weakening the assertion. The synchronization CLI fixture
deleted an API object and immediately deleted and recreated its bucket before
the asynchronous export removed the old mirror file. The retained file could
then be ingested into the new bucket. The fixture now rescans, waits and asserts
that the file is gone before deleting the bucket; the expected 404 is unchanged.
Before repair, the coordinator reproduced that race in three of 12 runs and
observed the retained file in each failing run. Both repaired binaries passed
20 consecutive runs before the final native suite; those repetitions are not
additional unique acceptance cases. Production behavior was unchanged.

Object deletion and same-name bucket recreation are not atomic with directory
export: a mirror file whose deletion has not yet been exported can be ingested
into the recreated bucket. Consumers needing absence across recreation must
wait for export and confirm the mirror file's removal first.

The final native result includes the existing ignored
`opted_in_pubsub_emulator_receives_the_notification` test. It requires an
explicitly started local Pub/Sub emulator and
`GCS_MIMIC_TEST_PUBSUB_EMULATOR_HOST`; it was not executed here. No test was newly
ignored to obtain the passing result.

Failure review separated obsolete fixtures from runtime defects. Historical
schemas do not retain immutable object owners, so migration fixtures now expect
an explicit null owner while retaining exact equality for all other fields.
Raw SQL-cost fixtures now include the owner-column migration without relaxing
their instruction limits or index checks. Bucket IAM/ACL synchronization changes
metageneration when it changes the bucket ACL; restart and denied-write tests
now compare against the complete resource after that authorized mutation.
GET `/` is the XML service route and requires an explicit project header, so
the former unknown-route assertion now checks its exact 400 XML error.

The runtime read defect concerns unsupported headers being rejected before
selected-generation authorization. Moving only the read handler's generic
validation left an upstream conditional-header validator in place: the first
repair attempt still failed the anonymous `If-Range` case, 400 instead of 401.
The subsequent authorized repair defers GET conditional-header validation to
the existing authorized read, without adding another lookup or restoring a
bucket-only admission check. The subsequent targeted regression passed below.

Authentication setup previously attempted a write transaction even when the
catalog contained no buckets or objects, preventing the seed publication tests
from reaching their intended barrier. A cached static existence query now
avoids that empty write transaction after installing the authentication profile;
an orphan object fails explicitly as catalog corruption. Nonempty catalog
reconciliation is retained. The new strict-profile test first failed because
its empty identity registry was invalid; using one valid identity then passed
the held-writer test for both authentication profiles. Existing seed and
nonempty reconciliation results are recorded separately from this fixture repair.
SIGTERM during authentication reconciliation of a nonempty catalog was not
tested for immediate cancellation or graceful draining. Startup still directly
awaits `configure_authentication` in `src/startup.rs`; the passing empty-catalog
seed cases do not establish that separate startup boundary.

The synchronization stack overflow was repaired by boxing the shared,
single-worker control future, retaining its existing authority scope and awaited
execution. The previously aborting case then passed on the standard test stack;
the ingestion module passed 15 cases and the mirror module passed 19. Those
checks overlap: the exact formerly aborting case is included in the 15 ingestion
cases, giving 34 unique ingestion/mirror cases. No stack-size increase or skipped
test was used. Later inspection of matching workspace artifacts observed the
`run_live_config` poll-frame reservation change from `0x67030` to `0x46a10`,
with a separate `0x20` prologue in each. This is a diagnostic observation, not
a claim about general execution speed or RSS.

The SDK lane's fixture-only repair check passed four library cases and the
authentication, copy, compose and hold HTTP binaries: 122, 14, 19 and six cases
respectively. Its 165 targeted cases and selected warnings-denied Clippy passed.
These are Rust repair checks, not executed SDK scenarios. The generation-change
compose fixture was made consistent with its ACL foreign keys; the replacement
source, retained destination, empty staging and foreign-key checks stayed in
place. Owner-based positives remain paired with nonowner and revoked-delegate
denials.

One targeted run inside the restricted execution environment failed to bind
local listeners: the first bucket binary reported one pass and 69 failures,
and Cargo stopped before the remaining binaries. This was an execution-boundary
failure, not acceptance evidence. In the authorized local-listener environment,
the bucket, initialization and foundation binaries subsequently passed 70, 10
and 28 cases respectively. The versioning fixture then exposed a second stale
expectation, hardcoded schema 42 versus current schema 44; it now compares the
upgraded version exactly with the current version captured before downgrading.
No negative test, test thread stack or ignored-test setting was relaxed.

After both GET validators were moved, the targeted range, metadata, versioning
and seed binaries passed 16, 6, 9 and 21 cases respectively. The selected ACL
read and immutable-owner authority modules passed nine cases, including
customer-key validation, public-access prevention, uniform access, selected
generations and owner revocation. The library repair checks passed 19 distinct
cases: five principal-profile/reconciliation cases, four historical migration
cases, two SQL-cost cases, two bucket revocation cases and six customer-key
cases. The 18 cases from the first library attempt and the corrected new
profile case establish those 19 results; reruns are not added to that count.
The bucket, initialization, foundation and library results precede the final
upstream GET-validator repair. They remain repair checkpoints, rather than
passing gates for its subsequent source. The range, metadata, versioning, seed
and selected ACL results were obtained after that final runtime repair.

The targeted commands used `env -u RUSTUP_TOOLCHAIN CARGO_INCREMENTAL=0
CARGO_BUILD_JOBS=2 cargo test --locked --offline` with the named integration
targets, and `--lib` with the named library/module filters. Whole-test type
checking passed after each runtime repair. After formatting only the owned
files with child-module traversal disabled, `cargo check --tests --locked
--offline` passed in 17.11 seconds, the owned-file rustfmt check passed, and
`git diff --check` passed. `target/debug/xtask lint` reported zero production
violations and one existing reasoned suppression. These targeted results are
repair evidence and are not added to the final native test count.

At the first native-passing manifest (`95d827…`), the static lane ran the
following existing checks after the coordinator released its native binaries:

- `bash scripts/check-docs.sh`: passed, including ledger validation, canonical
  English/Japanese matrix synchronization, release-note links and the mocked
  Docker Hub description checks. The inventories remain 606 feature rows,
  401 parameter locations and 357 schema fields; they are separate inventories.
- `bash scripts/test-container-release.sh` and
  `bash scripts/test-container-mirror.sh`: passed with mocked service commands,
  including immutable retries, attestations and error handling; no publication
  or remote mutation occurred.
- `bash scripts/test-check.sh` and `bash scripts/test-pre-push.sh`: passed the
  existing mocked orchestration, failure propagation and cleanup checks.
- `bash -n` over all 32 `scripts/*.sh` files: zero failures.
  `bash scripts/smoke-containers.sh --help` exited zero and
  `bash scripts/smoke-containers.sh linux/unknown` exited the expected two.
  Neither command ran container smoke scenarios.
- Existing markdownlint-cli2 0.24.0 / markdownlint 0.42.0: the evidence document
  and both roadmaps passed with zero diagnostics under their existing file
  settings. The Japanese roadmap also passed the existing prose flow checker.
  An additional default-rule audit of the two generated matrices failed with
  827 diagnostics: 819 line-length diagnostics and eight repeated headings in
  different parent sections. A temporary configuration, used only for those
  generated files, disabled line wrapping and limited duplicate-heading checks
  to siblings; that separate check passed with zero diagnostics. Generated
  bytes and repository configuration were not edited, and the default audit
  is not reported as passing.

The native collector command was
`target/debug/xtask release prepare aarch64-apple-darwin
target/debug/gcs-mimic <fresh-temporary-directory>/bundle`, with offline Cargo
metadata and incremental compilation disabled. Its package index covers a
superset of the locked workspace graph, including build, test and other-target
dependencies; it does not claim all 147 registry packages are linked into the
binary. All 274 indexed notice files were present and nonempty. The quick-xml
0.42.0 and httpdate 1.0.3 license texts matched their registry copies byte for
byte; both tonic-prost 0.14.9 supplied license copies matched the committed
upstream license text. `BUILD-INFO.json` records the dirty source and requested
Darwin target and verifies only the binary's version output. No release binary
was built or published, and the GNU/Linux-only `release notices` command was
not run on macOS.

The single `target/debug/xtask dependencies check --scope all` execution exited
one. The exact Composer 2.11.1 index remains
`sha256:537fb03057ed901ae06bde7ef25af348903485ae1acc7fc789b80d8bac666f35`,
while the floating latest index is
`sha256:4ed71c38c6d788eb4e4e3747b5332f5b40d5b5a6fcadd5062a1a70986a118e0a`.
The existing [dependency policy](../dependency-policy.md) and
[refresh evidence](dependency-refresh.md#refresh-2026-10-31) explain the stale
riscv64 publication and the 2026-11-06 review deadline. This is the only reported
available update; the remaining checked compiler, direct dependency, SDK,
toolchain, container and CI inputs reported current, while fake-gcs-server
remains an explicitly pinned comparison reference. The existing exception is
a deviation from the freshness target and does not convert this exit to a pass.
No dependency update, installation or automatic refresh was performed.

The required global tracked-file local-path scan found one match in a historical
planning instruction containing the scan command itself. It did not identify
an actual local environment path. The updated evidence document contained no
local paths or task/review management identifiers.

The old manifest's end-of-lane verification failed: 789 matching inputs and
five changed files, plus the checksum tool's warning line. The changed files
were the Go strict-authentication fixture, the gRPC memory regression tests,
both gRPC version adapters and the gRPC status mapper. The coordinator confirmed
these were its authorized SDK repairs. An initial progress message incorrectly
reported an unchanged end manifest before inspecting the result; it was promptly
corrected to this failure. The lane stopped source-dependent checks and does
not transfer its earlier passes to the changed inputs. The subsequent repeated
checks on the repaired inputs are recorded below.

The first official SDK command on the native-passing manifest was
`CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=2 GCS_MIMIC_SDK_SCHEME=https
bash scripts/sdk-go.sh --with-ruby --with-node --with-python --with-php`.
It exited one after 12 seconds: 86 of 89 top-level Go tests passed, three
failed and none were skipped. `TestCrossProtocolObjectsStrict` and
`TestGRPCUpdateCopyStrict` received gRPC `PermissionDenied` for anonymous
requests where `Unauthenticated` was expected; the former contained nine
affected assertions. `TestStrictAuthObjectCreator` expected 403 for attributes
of its own upload and received success. The script stopped before Ruby, Node.js,
Python and PHP; extra clients, the race scenario and strict Rails were not
executed. The SDK lane stopped and waited for both owned servers and verified
794 matching inputs before and after its run. Historical SDK passes are not
substituted for these failures or omissions.

Object permissions are evaluated after generation selection, so anonymous
catalog denials bypassed the former bucket-stage gRPC status translation. The
coordinator added the JSON adapter's anonymous-denial rule to all 24 gRPC v2
wrappers and the v1 object get/update wrappers. A new two-version regression
checks anonymous `Unauthenticated`, authenticated-reader `PermissionDenied`
and unchanged objects; disabling the repair made it fail, and enabling it
passed. That targeted result does not establish the repeated full native or
SDK gate. The creator fixture was updated to the existing immutable-uploader
OWNER contract outside uniform access: it now verifies exact own-object
attributes and bytes while retaining overwrite denial and adding owner-delete
denial. No production owner permission was broadened by that fixture repair.
Independent static review identified two unresolved status-mapping gaps outside
the verified cases. Terminal errors emitted inside v2 `BidiWriteObject` by
`receive_write` or `receive_append` can bypass anonymous translation; an
anonymous overwrite of an existing object under an `allUsers` object-creator
grant can therefore retain `PermissionDenied`. Delayed catalog denials in v1
`ComposeObject` and `InsertObject` can also retain `PermissionDenied` for an
anonymous caller. Authorization still denies these operations; the gaps concern
the returned status code. These findings were inferred from source and were not
reproduced in this run. They remain explicit follow-up work, without changes to
negative expectations or the immutable-owner contract.

The SDK lane subsequently ran the following commands against the current
accepted manifest, with Cargo incremental compilation disabled and build jobs
limited to two. Each exited zero; none of the failed attempt's counts are added.

| Command | Actual final result |
| --- | --- |
| `GCS_MIMIC_SDK_SCHEME=https bash scripts/sdk-go.sh --with-ruby --with-node --with-python --with-php` | 25 seconds. Go: 89 top-level tests passed, zero failed/skipped. Ruby completed permissive/strict and V2/V4 signing scenarios, including each profile's six buckets, nine objects, derived bytes and three resumable chunks. Node.js: 59 passed, zero failed, one existing skip. Python: 50 passed across its four groups. PHP: 57 passing markers. |
| `bash sdk-tests/java/run.sh` | Five seconds. Dedicated permissive HTTP server, existing warm Maven cache; multipart, PATCH, resumable, generation, read/list and frozen dependency-tree comparison passed. |
| `bash sdk-tests/dotnet/run.sh` | Two seconds. Locked restore/warm cache on the same HTTP server; resumable, metadata, generation, conditions and read/list passed. |
| `bash sdk-tests/rust/run.sh` | Four seconds. Official Rust and Arrow scenarios passed on HTTP and a distinct h2c gRPC listener. Guard counters: Rust HTTP 5 / gRPC 4, Arrow HTTP 6 / gRPC 5; blocked zero. |
| `(cd go && GOTOOLCHAIN=auto go test -race -tags integration -count=1 -v ./...)` | 24 seconds. 32 top-level tests passed: 11 guard and 21 facade cases, including Unix/listener, parallel execution and cleanup; zero failed/skipped. |
| `(cd go/sourceexport && GOTOOLCHAIN=auto go test -race -count=1 -v ./...)` | Three seconds. Eight top-level tests passed, including writer barriers, failures and write/sync preservation; zero failed/skipped. |
| `bash sdk-tests/ruby/active_storage/run-strict.sh` | Two seconds. Signing self-check and three strict Rails model scenarios passed: JWT upload/download and V2/V4 direct-upload CORS; zero failed. |

The lane verified 794 matching inputs and zero nonmatching entries before
execution, after the HTTPS command and after all client checks. Owned servers
were stopped and waited for. Normal CA/SAN validation and wrong-CA, wrong-host,
signature-tampering and authorization negatives remained enabled. The Node.js
skip covers a strict-only authorization case inside the permissive profile;
the strict profile passed it. Existing listener-count, credentials-deprecation
and deprecated-package warnings remain visible in the logs.

Observed toolchains/SDKs were Go 1.27.3 / Storage 1.70.0; Ruby 4.0.9 / Bundler
4.0.23 / Storage 1.62.1; Node.js 26.13.0 / npm 11.20.0 / Storage 8.4.0;
Python 3.15.2 / uv 0.13.6 / Storage 3.17.1; PHP 8.5.12 / Storage 2.6.1;
Temurin 27.0.1.0.2+1 / Maven 3.10.0 / Storage 2.76.0; .NET SDK 10.0.403 /
Storage 5.0.0; Rust Storage 1.21.0 / Arrow 0.14.2; Rails 8.1.4 / googleauth
1.17.6. These observed versions describe this run, not a second freshness check.

The HTTPS virtual-host probe established rejection of an unsupported SAN; it
did not establish successful virtual-host transfer. The additional client
harnesses used permissive HTTP, and Rails used a strict HTTP model scenario,
not browser UI. This evidence does not establish real-cloud or metadata-server
execution, eight actual consumer applications or complete Go source/API parity.
Java and .NET artifacts were released to the image lane for reuse without
another client rebuild.

The final image lane verified the accepted 794-input manifest at its start and
end: 794 matches, zero nonmatching entries and zero exit status. Its separate
Docker COPY input list covers 366 files plus `Dockerfile` and `.dockerignore`,
SHA-256 `d2e47d5f84bebf17e31e313a1a7859d42325c10fc2d80d7374a37b1f168f9d41`.
That input list compared identically before building, after each architecture
and after all scenarios. Each architecture was built once with exit zero;
the five smoke cases per architecture used the resulting prebuilt image and
did not rebuild it. BuildKit's maximum provenance retained the existing
Dockerfile frontend, Rust/runtime pins and COPY context inputs.
The recorded build and smoke invocations were as follows. Image tags and
metadata paths are represented by environment variables for portability and
refer to the actual image identities below.

```sh
CARGO_INCREMENTAL=0 BUILDX_METADATA_PROVENANCE=max docker buildx build --builder desktop-linux --platform linux/arm64 --load --tag "$ARM64_IMAGE_TAG" --progress plain --metadata-file "$ARM64_METADATA" .
CARGO_INCREMENTAL=0 BUILDX_METADATA_PROVENANCE=max docker buildx build --builder desktop-linux --platform linux/amd64 --load --tag "$AMD64_IMAGE_TAG" --progress plain --metadata-file "$AMD64_METADATA" .
bash scripts/smoke-containers.sh --image "$ARM64_IMAGE_TAG" linux/arm64
bash scripts/smoke-containers.sh --image "$AMD64_IMAGE_TAG" linux/amd64
```

The unchanged Dockerfile retained release build jobs limited to two and its
default disabled incremental compilation.

| Platform | Docker inspect ID / OCI index digest | Expanded image bytes | Runtime user |
| --- | --- | --- | --- |
| `linux/arm64` | `sha256:841e4610e61680f3e4474b1dc6a73202863c828d1670045432ea615cfc86903c` | 99270747 | `10001:10001` |
| `linux/amd64` | `sha256:71658f7fa97d29831409e86efa1e2e98d96632248e48a368f533d3b02fd2e3b9` | 91091257 | `10001:10001` |

The ARM64 platform manifest is
`sha256:d67d8230b8b7aa7b57f5bb59edd9b7b938a3a240b0c04626337bebfdd7dbdff3`,
with config `sha256:1645d203e37e14fa23c51b9d16b022d7ff418454929ab880419bd164b44db99f`.
The AMD64 platform manifest is
`sha256:8f7e2290a3b89a20c979a0d4398523b73390ba3b71dcd13fe01e0e98cbb7ef97`,
with config `sha256:94d0789a916c2e967d80cfb1b52c9899d6cef20a379b3ab3c7048df313452e26`.
The image byte counts are expanded image sizes, not process RSS or transferred
bytes, and are not a performance comparison.

Java, .NET and Node.js generic Testcontainers each executed one scenario on that
same ARM64 image, all exiting zero. Java reported Maven build success and its
multipart/PATCH/resumable/generation/read/list scenario. .NET verified
resumable/metadata/generation/conditions/read/list; Node.js verified startup,
health, mapped resumable Location and exact bytes. Node.js locked setup also
passed; its two existing PassThrough listener-count warnings remained visible.
No owned Testcontainers container remained after cleanup.
The existing JDK/Maven, .NET and npm cache locations were supplied through their
normal environment variables; no local tool/cache paths are required here.
The executed client entrypoints were:

```sh
GCS_MIMIC_IMAGE="$ARM64_IMAGE_TAG" bash sdk-tests/java/run.sh --container
GCS_MIMIC_IMAGE="$ARM64_IMAGE_TAG" bash sdk-tests/dotnet/run.sh --container
(cd sdk-tests/testcontainers/node && npm_config_cache="$NPM_CACHE" npm ci --ignore-scripts --no-audit --no-fund)
(cd sdk-tests/testcontainers/node && GCS_MIMIC_IMAGE="$ARM64_IMAGE_TAG" node scenario.mjs)
```

Trusted headless Chromium then exercised the same ARM64 image:

| Browser profile | Initial assertions passed | After-restart assertions passed |
| --- | --- | --- |
| Strict CORS | 38 | 7 |
| Permissive CORS | 36 | 7 |
| Strict signed URLs | 51 | 10 |

All six executions exited zero, totaling 149 passing assertions and zero
failures. This is the new accepted-manifest run, separate from the historical
149-assertion run below, and represents six executions rather than 149 browser
processes. Each API restart used the same container and volume after SIGINT
shutdown with exit zero and OOM false. Final CA-verified bucket listing was
empty in both profiles; fixture objects and buckets had been removed.
The actual browser entrypoints used the following argument forms, with owned
container/bucket and generated manifest paths expressed as variables:

```sh
docker exec "$BROWSER_CONTAINER" bash scripts/cors-browser.sh https://gcs:4443 "$STRICT_CORS_BUCKET" strict initial
docker exec "$BROWSER_CONTAINER" bash scripts/cors-browser.sh https://gcs:4443 "$STRICT_CORS_BUCKET" strict restart
docker exec "$BROWSER_CONTAINER" bash scripts/cors-browser.sh https://gcs:4443 "$PERMISSIVE_CORS_BUCKET" permissive initial
docker exec "$BROWSER_CONTAINER" bash scripts/cors-browser.sh https://gcs:4443 "$PERMISSIVE_CORS_BUCKET" permissive restart
GCS_MIMIC_BIN="$NATIVE_BINARY_COPY" bash scripts/signed-browser.sh prepare https://gcs:4443 "$SIGNED_BUCKET" initial
GCS_MIMIC_BIN="$NATIVE_BINARY_COPY" bash scripts/signed-browser.sh prepare https://gcs:4443 "$SIGNED_BUCKET" restart
docker exec "$BROWSER_CONTAINER" bash scripts/signed-browser.sh run "$SIGNED_INITIAL_MANIFEST"
docker exec "$BROWSER_CONTAINER" bash scripts/signed-browser.sh run "$SIGNED_RESTART_MANIFEST"
```

The native helper copy used for signed manifests matched its source binary byte
for byte, SHA-256
`3ce64b06dc30f09565af9d21b66d321e65f3b2c26efcc0d77bfcd551a5fc2a20`,
and reported version 0.7.0; no new host Cargo build was run for the browser lane.

The API used `https://gcs:4443` without publishing its host port; pages used two
distinct HTTPS loopback origins. A private NSS database trusted only the
development CA with `C,,`. Normal CA/SAN verification remained enabled without
certificate-error bypasses or host trust-store changes, on an owned internal
network. The existing root-run browser helper used Chromium's `--no-sandbox`
inside its container; it did not disable certificate or web-security checks.
Observed versions were Chromium `156.0.8078.73-1~deb12u1`, NSS
`2:3.87.1-1+deb12u4` and CA certificates `20250419~deb12u2`; the browser base was
`sha256:1681862882da07ee5476fee047bcf34138833f1df42633bb25bcb0f438d5bf3d`,
with Node.js 24.22.1. Docker client/server were 29.9.2, Compose 5.6.1 and
buildx 0.38.1. API limits of one CPU, 256 MiB, read-only root and 32 MiB tmpfs,
and browser limits of one CPU, 1 GiB and 2 GiB swap ceiling, are configured
limits, not measured resource use.

These cases used disk storage, path-style URLs, headless Chromium and local
Docker. Testcontainers exercised a loopback HTTP/permissive subset. Strict
HTTPS Testcontainers, full RAM profiles, actual consumer/Rails UI applications,
other browsers, virtual-host SAN transfer, Google Cloud and physical host power
loss were not tested. The early failed shell wrapper and old images remain
earlier checkpoints and are not reused as current passing evidence.
Smoke resources were removed by their existing traps and no owned
Testcontainers container remained. The coordinator retained the stopped owned
API/browser containers, network and volumes for settlement cleanup. All three
owned API containers reported exit zero and OOM false. The browser helper's
PID 1 was `sleep infinity`; its container exited 137 after `docker stop`, with
OOM false, while all six Chromium harness executions had exited zero. This
helper shutdown status is retained separately from the browser assertion result.

After the coordinator released the current accepted manifest, the static lane
repeated `scripts/check-docs.sh`, the global path inspection, native package
preparation, selected license/index verification, owned-document Markdown and
`git diff --check`; all completed successfully within their stated scopes.
Checksum verification before and after these repeated checks found 794 matching
inputs and zero nonmatching entries. The native package again contained 147
registry packages and 274 nonempty indexed notice files; all five selected
license byte comparisons and comparison of the copied binary with the current
native binary passed. Its binary-origin and ABI limitations remain unchanged.
The path scan still found only the historical instruction's literal scan
command, with no local paths or management identifiers in the evidence document.
The coordinator did not require rerunning the source-independent mock/script
checks or the freshness query; their earlier results and the failed freshness
exit remain visible above.

## Acceptance map

| Surface | Feature records | Contract and evidence | Remaining integration boundary |
| --- | --- | --- | --- |
| Docker/Compose, legacy flags, HTTP/HTTPS, health and advertised/public origins | DESIGN-055, DESIGN-058–066, UX-001, UX-003 | [Configuration](../fake-gcs-server-configuration.md), [legacy aliases](../slices/legacy-setting-aliases.md), [runtime administration](../slices/runtime-admin-endpoints.md) | Final ARM64/AMD64 images each pass five smoke cases. Java/.NET/Node containers and trusted Chromium pass on that same ARM64 image. Published 0.7.0 lacks these source additions. |
| Media, multipart, resumable, gzip, ranges, conditions and metadata | JSON-OBJECTS-005, JSON-OBJECTS-007, BEHAVIOR-OBJECT-005–015 | Existing object/upload contracts, [HTTP ingress](../slices/http-ingress.md), [MD5 availability](../slices/object-md5-availability.md) | Keep provider deviations, post-EOF unknown outcomes and checksum/protection negatives. |
| HTTP batch and method override | BEHAVIOR-COMMON-008, AUDIT-010 | [Batch](../slices/http-batch.md), [ingress](../slices/http-ingress.md) | Native batch and actual Java method-override scenarios pass. Per-call errors and authorization are independent; a batch is not atomic. |
| XML list, copy and multipart | EXT-XML-003, EXT-XML-004, EXT-XML-011, EXT-XML-019, EXT-XML-020, EXT-XML-023, EXT-XML-024, EXT-BEH-001 | [List](../slices/xml-object-list.md), [copy](../slices/xml-object-copy.md), [multipart](../slices/xml-multipart-upload.md) | Arrow PUT/GET/range/list/copy passes. Broader XML fields, owner projections and provider-exact errors remain outside that subset. |
| Public access, CORS and signed browser URLs | DESIGN-002, DESIGN-030–044, DESIGN-051–053, UX-006–007 | Final trusted Chromium scenarios below, [strict local JWTs](../slices/strict-service-account-jwt.md), [client adapters](../slices/sdk-client-adapters.md) | CORS passes both profiles and restart; signed browser URLs pass strict mode and restart. Permissive signing does not verify cryptography; actual strict Rails UI and virtual-host browser certificates remain separate. |
| Go constructors, InitialObjects, explicit identity/metadata, clients, handlers and direct helpers | DESIGN-062, DESIGN-068, DESIGN-082 | [Subprocess facade](../slices/go-subprocess-facade.md), [Unix transport](../slices/unix-http-transport.md), [fixtures](../slices/local-fixture-administration.md) | A subprocess adapter is not literal in-process embedding. Ordinary Backend access, complete exported API parity and Windows NoListener remain unsupported. |
| Notification settings and effects | JSON-NOTIFICATIONS-001–004, EXT-BEH-032–033 | [Local Pub/Sub](../slices/pubsub-notification-delivery.md), [wire evidence](pubsub-notification-delivery.md) | Separately provisioned local emulator; bounded durable at-least-once delivery, no Google delivery/ordering guarantee. |
| Fixture replay, reset and reseed | DESIGN-062, DESIGN-068, UX-002, UX-009 | [Atomic seed/reseed](../slices/atomic-seed-reseed.md), [reset](../slices/local-instance-reset.md) | Administrative authority remains separate from IAM. Seed `.metadata` files are literal bodies, not implicit import sidecars. |
| Filesystem xattr/sidecar and memory snapshot migration | DESIGN-062, DESIGN-063, DESIGN-072–074 | [Import](../slices/import-filesystem.md), [source preservation](../slices/source-state-preservation.md), [snapshot](../slices/source-memory-snapshot.md), [Go capture](../slices/go-source-memory-export.md) | Quiesced source, separate disk destination and explicit cutover; filesystem normalization cannot recover lost names/history. Physical host power loss is not established. |
| Working-tree export, observation, deletion and recovery controls | DESIGN-072–074 | [Synchronization](../slices/directory-synchronization.md), [continuous observation](../slices/continuous-directory-observation.md), [deletion controls](../slices/external-deletion-controls.md), [durable conflict storage](../slices/sync-conflict-recovery.md), [live CLI](../slices/sync-cli.md) | The earlier storage/control/Linux checkpoint remains recorded. On 2026-10-31 nine native CLI cases and ten mock cases pass, including actual deletion approval and scoped operator recovery. Exhaustive platform, recovery and current-source combined gates remain pending. |
| Bounded RAM storage | DESIGN-077, DESIGN-070–074 | [Memory backend](../slices/memory-backend.md) | Body and SQLite page limits are separate from RSS. Native gRPC/appendable and bounded-catalog cases pass. The final multi-SDK/browser runs use disk; they do not establish every RAM SDK profile. Directory sync/import targets remain disk-only. |
| Go/Ruby/Rails/Node/Python/PHP and additional clients | DESIGN-082–087, AUDIT-001–002, AUDIT-004, AUDIT-008 | Final official HTTPS suites, [strict Rails](../slices/strict-service-account-jwt.md), [Java/.NET/Rust/Arrow/Testcontainers](../slices/sdk-client-adapters.md) | Declared native SDK, Rails model and three generic-container scenarios pass. Extra-client HTTPS/strict/signing profiles and actual consumer applications remain outside these passing cases. |

The ledger retains broader Google cases and all existing unresolved specification
gaps. A scoped implementation does not complete its parent method or the full
migration target. Physical platform/durability boundaries and unsupported Go
surfaces remain explicit even when native wire tests pass.

The [native write-back recovery](../slices/native-writeback-recovery.md) storage
checkpoint passes 32 write-back and 22 conflict tests. It verifies bounded
restart paging, retained original owners, full quota and paired catalog/tree
fault recovery. The later [protected-transfer run](../slices/protected-conflict-transfer.md)
passes 67 storage cases: those existing 54 plus 13 transfer/native-capacity
cases, including all 13 paired capture/transfer fault boundaries. These are
overlapping checkpoints, not additive independent test totals.
The [runtime control checkpoint](../slices/continuous-directory-observation.md)
records 19 mirror cases, 11 HTTP-control cases and 48 tests across six native
import/sync targets passing. Four final regressions cover post-displacement
failure with repeated old-handle rejection, eight export failure phases with
explicit recovery, retained owner-union authorization and typed 403 mapping.
The deleted-bucket repair/rescan also passes on the original standard thread
stack. These groups overlap other checkpoints and are not additive totals.
Component success does not declare the final source gate complete.

## Scoped source checkpoint: 2026-10-31

The [live CLI contract](../slices/sync-cli.md#native-operator-acceptance) records
the exact test IDs, commands, results and limits. Independent source review and
coordinator acceptance cover the five new operator journeys: three-page approval
of 100 frozen deletions; binary restore and empty-conflict discard with
generation/hold/replay checks; strict retained-original-owner authority through
bucket recreation/restart; confirmed handling of an over-limit sparse recovery
survivor; and a real committed restore whose response is lost without automatic
retry. The restore fix records an API-origin change so the restored bytes reach
the working tree, preserving ordinary ingestion's echo suppression and the
existing authority/atomic-publication checks.

After `cargo check --tests --locked --offline`, the final `sync_cli` native
target passes **nine**, CLI parser/mock tests pass **ten**, conflict storage
passes **22**, and write-back recovery passes **12**, with no failures or
ignores. The storage groups include their existing seam-fault and paired
PowerLoss snapshots. Production lint reports zero violations and selected
binary/test Clippy passes with warnings denied. These are overlapping scoped
checks, not a new whole-workspace total.

The sparse case constructs an Armed survivor in a stopped test catalog. Actual
server restart/protection/quota/authorization and native CLI discard then run
normally; the fixture does not prove the originating I/O fault. Sparse reads
check length and binary prefix, not the entire terabyte. Backup confirmation is
an operator attestation. Physical power loss, exhaustive watcher/platform/crash
behavior, live-server admission/503 timing and all parent cases remain pending.

The independent XML read checkpoint runs
`cargo check --locked --offline --test xml_bucket_http` followed by the same
target's tests: **five passed**, zero failed/ignored. It verifies GET Service's
project-bound listing, official first-1000 limit and V4 signed project header,
plus bodyless HEAD Bucket with strict permissions, virtual hosts and raw-path
signature binding. The [XML contract](../slices/xml-bucket-operations.md) records
retrieved official-source digests and the remaining inputs. Later PUT/DELETE
changes require their own checks; this five-case checkpoint does not cover them.

The subsequent accepted XML bucket-create/delete and object-delete checkpoint
passes **13 wire tests**, **two** bounded-document parser tests and **14**
signature tests after the target check. Selected library/binary/HTTP/signing-CLI
Clippy passes with warnings denied. The XML contract's verification table maps
each wire test to its exact fields, errors, digest/permission checks and
PAP/UBLA/hold/version/soft-delete/restart interactions. Earlier parser input
validation findings were fixed through quick-xml with explicit limits and
malformed-document no-publication tests. Parent XML methods remain PARTIAL for
their named valid-input, ACL, billing, authentication and integration cases.
This checkpoint does not claim a passing current-source whole-production lint
gate while another implementation is in progress.

The subsequent [multipart grammar correction](../slices/xml-multipart-upload.md)
reuses the lexical reader without imposing the bucket-creation tree limits.
The targeted check passes, followed by **eight multipart wire**, **13 bucket
wire** and **20 XML unit** tests. Malformed declarations, root-prefix confusion,
extra roots, DTDs and external entities preserve both prior destination bytes
and the uploaded part. A valid prefixed, escaped document completes through
one-byte HTTP chunks; separate unit cases retain 10000 parts and 2 MiB bounds.
The initial shared-catalog dead-code finding blocked strict Clippy; a subsequent
API/parser candidate passed targeted Clippy, and the existing XML PUT regression
target passed **13** cases after the official active-hold error-code correction.
The final namespace-reference cleanup passed type checking, but the ensuing
unit/production-lint rerun encountered shared copy-replay E0282/E0283 compile
errors. Later ACL edits require their own results; these scoped checkpoints do
not establish the final shared-source gate.
EXT-XML-020 remains PARTIAL with its original pending provider/integration cases.

The accepted [object ACL read scope](../slices/acl-permission-effects.md) adds
generation-bound READER/OWNER user/public grants independently of bucket IAM.
The focused read, gRPC, derived and BidiRead cases pass **nine wire tests**;
admitted-read revocation and copy's streaming/publication checkpoint also pass.
Copy replay records preserve the actual source generation, and every new
BidiRead RPC rechecks that generation after releasing the handle-table mutex.
Different-principal reuse, revocation, PAP/UBLA, CSEK, private live replacement,
legacy completion records and opened IAM snapshot compatibility are covered.
Check, production lint and targeted library/binary/ACL Clippy pass with warnings
denied. Thirteen authentication unit cases also pass, including registration
without bearer credentials and rejection of empty/unknown tokens and JWTs.

At that object-read checkpoint, the whole target still contained failing future
bucket cases. The subsequent accepted bucket/mirror source now passes all
**sixteen** ACL wire cases. Bucket roles use the pinned legacy IAM permissions;
ACL and legacy IAM writes synchronize atomically while preserving unrelated
grants, original local IDs, etags and PAP/UBLA semantics.

Independent review found and the owner reproduced an alias-reassignment leak:
every restart merged the stale ACL view into new IAM authority. Schema43 adds a
per-bucket marker so old independent grants merge once; subsequent restarts
only reproject current IAM. The corrected wire case denies the other principal
after both the first and second restart. Three marker units verify partial
startup recovery, new/recreated buckets and old buckets without an ACL row;
two rollback units preserve the original policy, ACL and unset marker on failure.
Two startup units retain the typed catalog/cleanup causes and correct diagnostics.
Targeted compilation, Clippy and production lint pass; the production gate has
zero violations and one existing reasoned suppression.

The accepted owner foundation adds schema44 nullable ownership independent of
ACL text. Three real-server cases and three owner units cover immutable IDs,
selected-generation read authority, owner-preserving ACL edits, current trusted
display aliases, full echoes, reused aliases with new explicit grants/revocation,
unknown legacy/aliasless owners and interrupted/repeated startup reconciliation.
The implicit OWNER exists only in the view, so an old alias is never stored as
new independent authority. After record/validation helper extraction, the full
ACL target passes **nineteen** cases, administrative publication passes
**seventeen**, and both existing upload Snapshot/PowerLoss cases pass. These
post-extraction runs are separate from the earlier sixteen-case checkpoint.
Compilation, targeted Clippy and production lint pass for that source.

The subsequent accepted JSON OWNER subset checks exactly object get, update,
getIamPolicy and setIamPolicy on the selected generation, including additional
OWNER revocation after body admission. Per-generation full projection never
treats an admission context as a grant. Public OWNER/PAP/UBLA and separate
bucket/create/delete/retention denials remain enforced. The implicit owner
consumes one of the 100 logical grants even without a display alias; oversized
publication and ACL edits preserve metadata, bytes and generation allocation.
The expanded real-server target passes **twenty-eight** cases, and one catalog
snapshot unit passes on disk/RAM. Existing ACL CRUD, object metadata and IAM
targets separately pass eleven, six and twenty cases at their recorded
checkpoints; these counts overlap the expanded target's behavior coverage.

Typed upload/copy snapshots bind the original bucket, name, generation and
incarnation, recheck current controls and never use a revoked delegate's saved
ACL as authority. The immutable uploader can replay its original ACL/owner
after destination generation deletion, using the current canonical alias;
copy replay still rechecks its source. The stored completion JSON shape is
unchanged. Resumable records are canonicalized before rendering, but full
resumable ACL/owner completion and retry acceptance remain pending.

The accepted publication correction preserves a moved object's immutable owner
and effective source ACL, including the legacy virtual default, without passing
through the current default's 100-grant limit. Matching notification settings
produce exactly Delete/Finalize messages after success; conditions or an ACL
write failure leave metadata, bytes, allocator, journal and queue unchanged.
The whole ACL target passes **31** cases and the existing move target **seven**.
Named copy/rewrite/compose and restore cases retain their declared ownership
rules; appendable finalization preserves its creator rather than its finalizer.
All **18** appendable cases, **17** administrative publication cases and both
upload Snapshot/PowerLoss cases pass at the publication checkpoint.

The overlapping PowerLoss test failure was a test-model defect: only the first
ledger received updates, and firing order could select another arm's baseline.
Each armed fault now owns its baseline and receives every matching seam update.
The new model unit passes with different arm times, reverse firing, skipped hits
and all modeled file/directory operations. Independent single-arm recovery
already passed; no production fsync defect or physical power-cut guarantee is
inferred. The publication changes add no filesystem write, rename or sync step.

DESIGN-017 remains PARTIAL. Complete ownership propagation and remaining
protocol projections, trusted membership, upload-snapshot ceilings/rebinding,
default/canned publication and remaining UBLA acceptance stay pending. The
named subsets do not complete every restore/move/import/appendable path.
DESIGN-020 retains its broader pending grant case while recording read
suppression separately. Integrated production lint and final workspace,
SDK/container results require their own current-source evidence; no result is
inferred from these native checks or from earlier integration artifacts.

The accepted [legacy V2 subset](../slices/xml-v2-hmac-signatures.md) adds
literal-secret HMAC-SHA1 query and GOOG1/AWS headers, trusted virtual-object
resources, current authority and named bucket/service methods. Google query
and legacy header canonicalization remain separate. Independent review found
unsigned header generation selection and literal-plus decoding defects; their
fixes preserve historical/current resources and exact response overrides.
Incorrect virtual-bucket signatures remain 403, while correctly signed but
unsupported bucket queries return 400. The final production-source checkpoint
passes **58** library, **78** wire and **68** authentication cases, **204** total
with none ignored, followed by a test-only recheck of the same nine V2 cases.
Final independent compilation and three boundary wire cases also pass; those
reruns are not added to the unique total. Targeted Clippy, production lint,
formatting and the artifact's three-document Markdown check pass. The V2 parent
and SPEC-V2-LEGACY retain broad HMAC/virtual-host, POST/resumable, .NET,
encoding, provider and SDK questions. These results do not validate later
publication changes; their separate correction and test-model evidence are
recorded above.

The accepted [V4 HMAC/header verification core](../slices/xml-v4-signatures.md)
decomposes algorithm/key state, exact credential/time boundaries, canonical
request fields, body integrity, current IAM/ACL authority, resource caps and
typed backend failure. Its final dedicated wire target passes **11** cases;
the unchanged XML/PUT regression passes **35**. Recorded supporting groups pass
**45** library, **58** wire and **33** signing cases, with overlap between groups.
The published AWS vector and local curl Google/AWS signatures independently
verify HMAC derivation. The closed-catalog unit verifies a retained backend
cause and XML 503 without an anonymous fallback; it is not a wire outage test.
Final targeted Clippy and production lint pass after shared caller integration.

DESIGN-031 completes its declared query-verification cases at the same granularity
as DESIGN-030. DESIGN-039 remains PARTIAL. Its initial ordinary-header checkpoint
still rejected the declared Google chunked path. The subsequently accepted
[signed-chunk ordinary-PUT subset](../slices/xml-signed-chunked-upload.md)
implements Google's directly linked AWS4 HMAC marker, seed/data/zero chain,
exact decoded length, actual EOF, decoded MD5 and encoding-token normalization.
Independent source review and seven independently executed wire cases pass.
The owner's 51 library, 69 related wire and 68 authentication/XML cases pass
before the final shared helper extraction. After that extraction, targeted
compilation, three vector/failure units, seven streaming wire cases and the
production lint gate pass again. Those ten rerun cases overlap the earlier
188-case set; the latter is not a final whole-workspace source gate.

Only the scoped AWS ordinary-PUT child cases are accepted. The original
V4-HEADER-CHUNKED-UPLOAD and SPEC-V4-HEADER-CHUNKED remain pending/open for
provider-exact no-encoding interpretations, RSA/query/trailer formats, other
operations, optional payload semantics and strict SDK interoperability.
Google identity propagation, query ambiguity/errors, SDK hash substitution,
remaining form policies and resumable initiation retain their named cases.
No full workspace/SDK/container or real-provider result is inferred here.

The subsequent V4 form-policy core uses the exact submitted base64 policy,
the same bounded verifier and current ACTIVE HMAC-key lookup, then the existing
signer authorization and bounded publication path. Independent source review
and the following focused command pass **three** native cases:

```sh
CARGO_INCREMENTAL=0 cargo check --bin gcs-mimic --test xml_post_policy_http --locked --offline
CARGO_INCREMENTAL=0 cargo test --test xml_post_policy_http v4_ --locked --offline
```

The wire cases cover RSA/HMAC binary uploads on disk/RAM, custom metadata,
key revocation and disk restart, registered signer authority, and rejected
tampering with unchanged public object bytes, metadata and catalog visibility.
The first sandbox execution failed all three cases at listener binding and
never reached request validation. The permitted loopback execution passes
three cases; it is one source checkpoint, not six independent cases. The owner
also reports the eight existing RSA form cases passing at its V4 checkpoint;
that group is not the independent three-case execution above.

The next source checkpoint adds legacy V2 RSA and HMAC-SHA1 form verification.
Independent targeted compilation passes, followed by **two** passing cases:

```sh
CARGO_INCREMENTAL=0 cargo test --test xml_post_policy_http v2_ --locked --offline
```

The RSA case verifies exact base64 signatures, version-specific field coverage,
an optional bucket condition, eight-day expiration, mixed/partial credentials
and preserved state after rejected uploads on disk/RAM. The HMAC wire case
verifies the literal secret, current ACTIVE key, revocation/restart and rejected
overwrites. Its complete core acceptance additionally has a passing independent
RFC2202 SHA1-vector unit; that unit is separate from the two-wire-case execution.

The final variant checkpoint passes **62** related wire cases, **68** existing
authentication/XML regressions and **48** selected library cases. The wire group
includes nine POST cases covering four-variant size boundaries, creator-only
overwrite denial, scoped IAM revocation under PAP/UBLA, mixed credentials,
201 XML and the existing unchanged redirect. The library group includes the
four-variant initial/tail write, flush and sync failures, closed-catalog typed
503 and RFC2202 vector. Its earlier twelve-unit run overlaps this library group.
Exact commands, primary-source digests and remaining cases are recorded in
[XML signed POST policy variants](../slices/xml-post-policy-variants.md).
Targeted Clippy, production lint, formatting and three-document Markdown lint pass.

DESIGN-041's V4 HMAC, V2 RSA/HMAC cores and named boundary/failure subsets are
accepted. Remaining credential/validator and version interactions, complete
authority, interrupted network bodies, strict clients, form fields and browser
behavior retain pending cases.
The no-policy Authorization-header alternative still needs reconciliation with
the official form contract's anonymous description. The parent remains PARTIAL
and its historical permissive SDK results do not validate the new strict HMAC
profile or later source.

The [dependency refresh](dependency-refresh.md#refresh-2026-10-31) separately
adopts Composer 2.11.1 and records limited ARM64 runtime/locked-install/self-check
results. Earlier PHP suites used 2.11.0. Neither those historical suites nor the
earlier image/browser results validate the new runtime or later parser changes.

The existing `cargo xtask release prepare aarch64-apple-darwin` collector was
also run with the current debug binary and a temporary output directory.
Its license index includes quick-xml 0.42.0 with SPDX `MIT` and
`LICENSE-MIT.md`; the generated text is byte-identical to the registry package's
license. This verifies notice collection, not a release build, binary provenance,
ABI or another platform.

## Central checks executed

On 2026-10-30:

- `cargo check --workspace --all-targets --locked --offline` passed for the
  integrated checkout at this checkpoint. This is not the final source gate.
- `cargo test --locked --offline --bin gcs-mimic sync_cli` passed ten mock-wire
  cases: explicit approval revision, session-bound findings, bounded wait/grace,
  opaque paging, local endpoint
  restrictions, chunked JSON/non-success statuses, oversized/truncated bodies,
  canonical flag forms, conflict paging and exclusive resolution/confirmation.
  That checkpoint's workspace run additionally passes all four `tests/sync_cli.rs`
  cases: live permissive rescan/wait/paging and stale 409s, disabled-sync 404,
  strict missing/invalid/insufficient credentials and token-file precedence,
  and a native CLI process against a controlled local 503 receiver. The latter
  checks JSON, nonzero exit and no automatic retry; it does not establish the
  running server's admission behavior or successful deletion approval. The
  latter is covered separately by the 2026-10-31 native operator checkpoint.
- `cargo check -p xtask --all-targets --locked --offline` and
  `cargo clippy -p xtask --all-targets --locked --offline -- -D warnings` passed.
- `cargo test -p xtask --locked --offline` passed 60 unit, 11 compatibility and
  two dependency integration tests, including numeric-release edge cases and
  all additional manifest/lock detectors.
- Go facade `go build ./...`, `go vet ./...` and
  `go test -tags integration -run '^$' ./fakestorage` passed. Bucket helper mock
  tests passed with the race detector, including exact three-field create,
  matching duplicate no-op, different-version rejection and backend-failure panic.
  The later real Rust/SDK integration run passes as recorded below.
- `cargo xtask dependencies check` reached all fifteen declared SDK manifests,
  including both Go adapters, the standalone Rust graph, Java/Maven/plugins,
  Temurin, NuGet/.NET and Node Testcontainers. Additional direct packages and
  runtimes were CURRENT. The fixed fake-gcs-server reference was reported
  PINNED_REFERENCE with the latest release separately. Exit **1** remains for
  the existing Composer latest-index publication-lag exception, due for review
  on **2026-11-06**; this is not an all-current result.
- `actionlint .github/workflows/ci.yaml .github/workflows/dependency-check.yaml`,
  `just --list` and `bash -n scripts/sdk-go.sh` passed. Remote CI execution is
  not established by these local syntax checks.

## Final-source native gate

The coordinator's final workspace run for the earlier integration source
exited zero. This is historical evidence for that source, not a current-source
gate for the subsequent XML/operator changes:

```sh
env -u RUSTUP_TOOLCHAIN CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=2 GCSM_TEST_JOBS=2 \
  bash scripts/with-test-tmpdir.sh bash scripts/test-rust.sh
```

All 60 test binaries passed: **1674 passed, zero failed, one ignored**. The
ignored test requires an explicitly provisioned local Pub/Sub emulator and is
not counted as executed. Doc tests add **four passed, zero failed**. Final
workspace all-target clippy with `-D warnings`, formatting and diff checks pass.
Production lint reports zero violations with one existing reasoned suppression.
The final Windows and Linux GNU/musl cross lanes also pass their clippy and
production gates. No thread-stack or streaming-buffer increase was used.

## Additional native and SDK execution

The final SDK runs use the latest frozen runtime, including the SDK-driven API
repairs. The Linux 306-case run verifies the import/sync/Unix/memory checkpoint;
those I/O implementations are unchanged by the later API repairs. Their final
API behavior is covered by the host's 1674-case gate, final SDKs and final Linux
image checks. Conditional-compilation repairs preserve Unix expressions and
behavior; this does not imply byte-identical source snapshots or Windows
runtime verification.

| Run | Actual result and boundary |
| --- | --- |
| Linux native, nonroot UID 10001 | **306 passed, zero failed, zero ignored**: 246 selected library cases and 60 cases across eight import/sync/Unix/memory targets. The source mount is read-only and the container has no network. CPU 2, memory 4 GiB, jobs 2 and bounded temporary storage are retained. Modeled power-loss seams pass; physical power loss is untested. |
| Windows and Linux GNU/musl compilation | Workspace/all-target check, clippy and `scripts/check-cross.sh` pass. This is compilation evidence, not Windows runtime execution or Windows NoListener support. |
| Go facade with the real Rust subprocess | Race-enabled facade integration passes **21 top-level tests, 27 including subtests**, zero failures/skips, with ambient SDK metrics explicitly enabled. JSON reads and disabled client metrics keep facade requests local. The later guard package independently passes **11 top-level tests plus two subcases** with the race detector. These separate runs are not one combined count. The real-source exporter also passes its race-enabled run. |
| Go HTTPS official SDK | **89 top-level passed**, zero failures/skips, including V2 signatures and cross-protocol listing/metadata preservation. |
| Ruby HTTPS official SDK | Origin self-check, signing, migration, permissive and strict suites all pass. |
| Node HTTPS official SDK | Self-check 7, migration 2, permissive 23 and strict 27 pass; zero failures. One permissive case is intentionally strict-profile-only. The pinned SDK's `MaxListenersExceededWarning` remains visible. |
| Python HTTPS official SDK | **50 passed**: self-check 5, migration 1, permissive 20 and strict 24. The pinned uv runtime and Python 3.15.2 are used. |
| PHP HTTPS official SDK | **57 PASS lines**: self-check/adapter 2, migration 1, permissive 23 and strict 31. V2/V4 signatures, checksums and streamed byte comparison pass. |
| Rails 8.1.4 strict local recipe | Signing self-check and **three runtime scenarios pass**: JWT upload/download and V2/V4 direct-upload CORS. Private unsigned reads and changed signed headers retain their rejection. This uses the Rails model/Net::HTTP fixture; an actual Rails browser UI has not run. |
| Java Storage 2.76.0 native HTTP | The unchanged official multipart, PATCH override, three-chunk resumable, generation, exact read/list and cleanup scenario passes after the scoped metadata `generation: null` repair. Maven dependency lock comparison, compile and execution all succeed. Strict/HTTPS/gRPC and container acceptance remain separate. |
| .NET Storage 5.0.0 native HTTP | The final normal runner passes three resumable chunks, exact read, metadata update with the returned ETag, generation/listing, create-if-absent rejection and cleanup. Default compression and If-Match remain enabled. The full JSON BaseUri and redirect guard are explicit. |
| Official Rust Storage 1.21.0 | The final normal runner passes bucket creation, forced resumable write, generation/metadata, bounded exact reads and cleanup. Guard counters: **HTTP 5, gRPC 4, blocked 0**. |
| Arrow object_store 0.14.2 | The independent final scenario passes XML PUT/GET/range/list, copy-if-absent and duplicate-copy rejection, with official control cleanup. Guard counters: **HTTP 6, gRPC 5, blocked 0**. This is third-party evidence, not official SDK coverage. |
| SDK-driven protocol regressions | Gzip initiation: four dedicated and ten existing ingress/multipart cases pass. Metadata If-Match: four dedicated, six metadata and sixteen read/range cases pass, plus two parser units. XML bucket decoding: fourteen wire cases and thirty-three signing regressions pass, including once-decoding, malformed/double/slash rejection and strict raw-path signature binding. These groups overlap the workspace totals. |
| Official local Pub/Sub emulator | The explicitly opted-in test passes: **one passed, zero failed, zero ignored**. It creates local topic/subscription resources, publishes a committed object notification, pulls and checks it, then deletes the fixture. [Notification evidence](pubsub-notification-delivery.md) pins the component and archive. |
| Shared test HTTP response framing | Check and test clippy pass; all 17 consumer targets pass **156 cases**, zero failures, one separately executed Pub/Sub opt-in excluded from the default run. The helper honors HEAD and Content-Length without waiting for connection EOF. The actual official emulator is the regression case. |

The five SDKs use the shared runner with both permissive and strict HTTPS
endpoints, normal development-CA/SAN verification and local-origin guards.
The Node changed-Host signature negative retains the original TLS server name;
independent wrong-CA and wrong-SAN negatives still pass. No verification or
authentication guard is disabled to obtain these results. SDK case totals
overlap native contracts and do not mean complete API compatibility.

## Final trusted-browser verification

The final ARM64 image is
`sha256:a36c261ac1dbfa12b586a7d3412a5c797e5ad9f3798d3f0282a6d21c969aed4e`,
independently checked with `docker image inspect`: Linux ARM64, UID/GID
10001:10001 and 98,557,226 image bytes. The isolated browser uses
`node:24.22.1-bookworm-slim` image
`sha256:1681862882da07ee5476fee047bcf34138833f1df42633bb25bcb0f438d5bf3d`,
Chromium `156.0.8078.73-1~deb12u1` and NSS tools `2:3.87.1-1+deb12u4`.

| Existing harness | Initial PASS lines | Restart PASS lines | Result |
| --- | ---: | ---: | --- |
| `scripts/cors-browser.sh`, strict | 38 | 7 | PASS, exit 0 for both phases |
| `scripts/cors-browser.sh`, permissive | 36 | 7 | PASS, exit 0 for both phases |
| `scripts/signed-browser.sh`, strict | 51 | 10 | PASS, exit 0 for both phases |

These are **149 assertion PASS lines**, zero failures, rather than 149
independent test processes. JSON and XML CORS differences, preflight and exposed
headers, browser denial of origin/method/header/credential mismatches, direct
media/multipart/resumable bytes, conditional 304 and strict permission denials
pass. Signed URLs bind `https://gcs:4443` before use and remain unchanged through
browser fetch. Signature/header tampering, expiry, anonymous private access and
insufficient upload permissions retain their failures with unchanged object
state. Permissive mode supplies no cryptographic verification evidence.

Both containers share an owned `docker network create --internal` bridge.
The API has only the `gcs` network alias, **no host-published ports**, one CPU,
256 MiB memory cap, read-only root filesystem and a 32 MiB temporary filesystem.
The browser has one CPU, a 1 GiB memory cap and a 2 GiB combined memory/swap cap.
These are configured container bounds, not measured RSS or speed improvements.
Only the disposable browser's NSS database trusts the development CA with
`C,,`; normal CA and SAN verification stays enabled and host trust is untouched.
Browser page origins are `https://127.0.0.1:54613` and `:54614`. Chromium's
root-container `--no-sandbox` flag is confined to that disposable fixture;
no certificate, hostname or web-security bypass is used.

The existing harness calls use `https://gcs:4443`, disposable
`cors-browser-migration-*` and `signed-browser-migration-strict` buckets and
`initial`/`restart` phases. Signed preparation runs the native offline CLI and
writes a temporary manifest accessible through a read-only checkout mount.
Each restart uses SIGINT, observes exit zero without OOM, then starts the same
container with the same named data volume. Persisted rules and exact bytes pass
before fixture deletion. A final CA-verified HTTPS bucket listing is
`{"kind":"storage#buckets"}`, with no items. All owned containers, volume,
network, browser-preparation image and signed manifests are removed afterward;
shared final images, base images and build caches are preserved.

Actual strict Rails browser UI, virtual-host browser access requiring additional
certificate SANs, other browsers, real Google Cloud and hardware power loss
remain unverified. The Rails model/Net::HTTP scenario is separate from these
browser harnesses.

## SDK contract boundaries

Java's actual JUL capture establishes its explicit body `generation: null`.
SPEC-015 records that narrow SDK-backed interpretation; dedicated wire tests
preserve other read-only null, changed-generation, selector, precondition and
strict authorization negatives. The .NET SDK's returned-ETag metadata mutation
condition is an atomic local compatibility extension under SPEC-METADATA-IF-MATCH.
Google's JSON header reference establishes GET support only; real-Google metadata
write parity is unverified. [The scoped contract](../slices/json-metadata-if-match.md)
retains unsupported other write conditions and standalone dates.

The official Rust SDK requires HTTP and gRPC at one client endpoint. An explicit
test-only bounded frontdoor forwards to the product's separate fixed loopback
listeners, rejects foreign origins and retains the SDK's anonymous credentials.
Arrow independently uses explicit anonymous configuration and a once-decoded
bucket component; signatures bind the unchanged raw request path. Neither this
frontdoor nor the origin guards are an OS egress sandbox.

## Final container verification

The final native workspace gate includes all SDK-driven API repairs. The
additional SDK scenarios and the five official HTTPS SDK suites above use
that repaired source. The complete HTTPS runner exits zero with pipeline
failure propagation enabled:

```sh
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=2 GCS_MIMIC_SDK_SCHEME=https \
  bash scripts/sdk-go.sh --with-ruby --with-node --with-python --with-php
```

Use the pinned toolchains and a clean gem environment. The SDK process guards,
normal CA/SAN verification and strict permission/signature negatives remain
enabled. No fatal-pattern line appears, and each suite's actual summaries
agree with the case counts above.

Both final source builds and existing `scripts/smoke-containers.sh --image`
runs exit zero. Each architecture passes all five workflows: isolated health
and media CRUD, initialization mounts, explicit seed replay, implicit `/data`
seed and synchronization/export/restart/shutdown. SIGINT/SIGTERM, persistent
storage and resource limits retain their checks. All 339 Docker input hashes
match before and after the builds; the checksum-list SHA256 is
`ea549283881a18a3d72f0cf5530864d67a242581ea23b8366fc7546cb4b6f964`.

| Local image | Docker inspect image ID / OCI index | Expanded image bytes | Smoke result |
| --- | --- | ---: | --- |
| `gcs-mimic:integration-20261031-arm64` | `sha256:a36c261ac1dbfa12b586a7d3412a5c797e5ad9f3798d3f0282a6d21c969aed4e` | 98,557,226 | 5/5 PASS |
| `gcs-mimic:integration-20261031-amd64` | `sha256:79bc106ef4868d5d1b91e5139d9a06207a09283919f0d21b9cae068cc66275d8` | 90,361,322 | 5/5 PASS |

Both images use UID/GID 10001:10001 and the pinned builder/runtime digests in
the [Dockerfile](../../../Dockerfile). These are local source images, not
published registry releases. The platform manifest digests are
`sha256:0e67e705051c75c8fac9fec79c2556208b55cb1a5773db109c0a442ccc28bda6`
for ARM64 and
`sha256:149396a83d75b8544fd5b1d3d795a16e372c3a118d6749853ebbe3342bc2fdc1`
for AMD64. Image sizes are not download sizes or RSS measurements.

| Actual Testcontainers command using that same ARM64 image | Result |
| --- | --- |
| `bash sdk-tests/java/run.sh --container` | PASS, exit 0 and Maven BUILD SUCCESS; multipart/PATCH override/resumable/generation/read/list and cleanup pass. The pinned Exec plugin launches a separate JVM so normal Testcontainers/Ryuk shutdown owns its process lifetime. |
| `bash sdk-tests/dotnet/run.sh --container` | PASS, exit 0; resumable/metadata/generation/conditions/read/list and container disposal pass. |
| `GCS_MIMIC_IMAGE=gcs-mimic:integration-20261031-arm64 node scenario.mjs` in `sdk-tests/testcontainers/node`, after locked `npm ci` | PASS, exit 0; native startup, health, mapped Location, resumable and exact bytes pass. Two pinned Node SDK PassThrough listener warnings remain visible. |

The clients use random mapped loopback ports and advertise those final endpoints
before resumable initiation. No SDK, container entrypoint, checksum, condition,
origin guard or cleanup behavior is disabled. Docker Engine 29.9.2, Compose
5.6.1, Maven 3.10.0, Temurin 27.0.1.0.2+1, .NET SDK 10.0.403/runtime 10.0.14
and Node 26.13.0/npm 11.20.0 are the observed tools. No owned app/Testcontainers
containers, smoke volumes or networks remain. Final local images are retained
for the coordinator; owned temporary fixtures and logs are cleaned after
evidence handoff.

No real Google Cloud, actual consumer-application or strict Rails browser UI
execution, Windows runtime, package publication or performance improvement is
claimed. Broader API/profile/provider cases remain pending in the feature
ledger; all declared executed subsets above remain distinct from those limits.
