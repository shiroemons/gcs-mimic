# Compose migration guide

<!-- markdownlint-configure-file {"MD013": false} -->

Feature IDs: UX-012, DESIGN-061, UX-001. This slice changes documentation and the Go migration scenario only; server behavior, dependencies and startup ordering stay unchanged.

## Acceptance contract

| Case | Required observation |
| --- | --- |
| migration-compose-guide | English and Japanese guides replace five common flag groups, build the unpublished image from source, initialize empty buckets with a buckets file, explain one-time seed, browser transfers, cancellation 499 and per-worktree ports; symptom/cause/repair examples match implementation |
| migration-compose-walkthrough | Execute the final source-build Compose example once: inherited healthcheck, initialized buckets, API bucket creation, upload and exact read; a peer follows the advertised resumable Location |
| TestMigrationAppScenarioParallelBrowser | Twelve concurrent Origin-bearing raw HTTP uploads all succeed, return CORS headers and preserve distinct bytes on anonymous reads |
| migration-scenario-coverage | Map every operation in onboarding plan section 9 to existing Go/raw HTTP/Ruby scenario assertions; preserve source comparison limitations |

The Location decision is final: keep bind-derived defaults, explicitly advertise `http://gcs:4443` in the migration Compose example, add no startup warning and do not derive Location from request Host. The single advertised origin serves container SDKs; host/browser clients use their reachable origin. Only unsigned permissive session URLs may have their authority rewritten while preserving path and query. No design requirement changes.

## Scenario coverage

| Plan operation | Executable coverage |
| --- | --- |
| Go default resumable NewWriter and ChunkSize=0 | TestMigrationAppScenarioGo/default and /single-request; recorded transport checks chunk requests and Location |
| Attrs CRC32C/generation, NewReader | Both Go modes check checksum, size, generation and exact bytes |
| CopierFrom source generation + DoesNotExist, replay 412, generation Delete | Both Go modes check copy bytes/metadata, replay failure and selected deletion/missing state |
| Server initiation with X-Upload-Content-Type | migrationSession and TestMigrationAppScenarioBrowser/resumable verify inherited text/plain |
| DELETE cancellation | TestMigrationAppScenarioCancellation checks 499, subsequent 410 and no published object |
| Origin preflight, media POST, changed-Host resumable PUT + Content-Range | TestMigrationAppScenarioBrowser/media and /resumable |
| Anonymous alt=media and path-style reads | Both browser modes check bytes; JSON response also checks CORS |
| Concurrent browser uploads | TestMigrationAppScenarioParallelBrowser, twelve simultaneous raw HTTP uploads and reads |
| Ruby endpoint/anonymous bucket.file, create_file, delete | sdk-tests/ruby/migration_scenario.rb through https.rb --migration-scenario; existing evidence in migrate-app-scenario.md |
| STORAGE_EMULATOR_HOST, HTTP, initialized buckets | scripts/sdk-go.sh, newEmulatorClient and TestMigrationAppScenarioGo; Ruby endpoint guard |

No plan operation is missing after the parallel case is added. Raw HTTP tests establish wire headers, not real browser CORS enforcement. The prior source comparison failed copy replay, preflight and cancelled-session assertions; Ruby metadata also differed. See [the recorded comparison](migrate-app-scenario.md#source-comparison). Full source equivalence and code-free application migration are not claimed.

## Sources and remaining scope

Reviewed on 2026-10-25: [Google resumable uploads](https://docs.cloud.google.com/storage/docs/performing-resumable-uploads), [Google CORS](https://docs.cloud.google.com/storage/docs/cross-origin), pinned fake-gcs-server v1.56.1 commit `896ece51e49eed0946089f3089f6343855c0d106`, and the existing migration contracts. JSON Discovery remains revision `20261013`, SHA-256 `1f74b7a93b3d2872cc332fc20dafae1657fc255930ce336618bea3d0b4d95ae0`. SPEC-RSM-LOCATION, SPEC-RSM-EDGES, SPEC-RSM-AUTH-CONDITIONS and SPEC-CORS-EDGES remain open; explicit Compose configuration does not resolve provider parity.

UX-001 and UX-012 retain their broader pending publication/platform/full-guide cases and PARTIAL status. DESIGN-061 retains IMPLEMENTED for its completed host/peer scope; the additional migration acceptance also passes.

## Verification

Verified on 2026-10-25 (UTC) with Rust 1.99.1, Go 1.27.3, Storage Go v1.70.0/API v0.308.0 and unchanged lockfiles. `cargo xtask dependencies check` passed with all inputs CURRENT. Go scenario source is commit `a749577`.

| Command/check | Result |
| --- | --- |
| `GCS_MIMIC_GO_TEST_RUN=TestMigrationAppScenario bash scripts/sdk-go.sh` | PASS: Go vet and all five migration tests, including default/single-request SDK transfers, browser media/resumable, cancellation, native omitted-project diagnostic and twelve concurrent raw HTTP uploads with exact bytes and CORS |
| `docker compose up -d --build --wait` using the guide's source-build file and isolated project | PASS once: local Linux ARM64 manifest list `sha256:73e6225fbb5da450ee4eebf004de919bce3d22149098d1fae10a0824f17e6a3f`, with pinned Dockerfile base digests |
| Guide host commands | PASS: inherited image healthcheck, initialized assets/exports, API creation of scratch-assets, media upload/read, Origin preflight/upload/read, path-style bytes and cancellation 499 |
| Peer resumable Location | PASS: pinned curl 8.23.0 container on the same Compose network PUT to the advertised `http://gcs:4443` Location with Content-Range; host read returned exact bytes |
| `just check` | PASS once on final source: formatting, Clippy with warnings denied, workspace tests, ledger/generated-view checks and shell/release harness checks |
| `cargo xtask compatibility render` | PASS: 591 rows; English/Japanese matrices regenerated with no diff because their displayed statuses and fidelity are unchanged |
| `npx --yes markdownlint-cli2` on edited Markdown | PASS; long command/table lines retain literal URLs and values; ordinary prose retains the existing length rule |

The concurrency test clones the guarded SDK transport and raises only its two-connection client cap to twelve, keeping the origin guard and timeouts; otherwise it could conceal server admission failures. Every upload starts from one shared release channel and uses distinct payload bytes. No SDK/global server defaults change.

English/Japanese Compose and shell blocks are identical; the shared commands were executed once. Test containers/network were stopped and temporary guide files removed; the isolated named data volume is retained. Existing Ruby scenario evidence is reused because Ruby is unchanged. The source comparison was not repeated; its recorded failures still prohibit a full replacement claim. No real browser, real cloud, benchmark, second architecture, image publication or remote workflow was run. Cached base layers are allowed; this is not a remote clean-checkout or published-image verification.
