# Application migration scenario

<!-- markdownlint-configure-file {"MD013": false} -->

This test-only slice covers DESIGN-082, DESIGN-083, UX-003 and UX-007. Parent feature status is unchanged; this is a representative application workflow, not full fake-gcs-server compatibility. No application-specific names, dependencies or server settings are introduced.

## Acceptance contract

The existing `scripts/sdk-go.sh` runner starts an isolated HTTP server and initializes `assets` and `exports` from a temporary version-1 buckets file. The existing Go origin guard, `STORAGE_EMULATOR_HOST` client construction and Ruby anonymous endpoint guard remain in use. Neither SDK creates the scenario buckets through the API.

| Case | Required observation | Result |
| --- | --- | --- |
| Go default Writer | Leave ChunkSize unchanged; a body above the default 16 MiB threshold uses a resumable Location and chunk requests | PASS |
| Go single-request Writer | ChunkSize = 0; upload, Attrs CRC32C and positive generation, exact NewReader bytes | PASS |
| Go copy and deletion | CopierFrom selects the source generation and DoesNotExist destination; replay returns 412, then generation-specific Delete removes source and copy | PASS |
| Server resumable initiation | X-Upload-Content-Type survives publication without a chunk Content-Type; DELETE cancellation returns 499, subsequent session access returns 410 and no object is published | PASS |
| Browser wire flow | Origin-bearing preflight and media POST succeed; a server-created session accepts a changed Host with PUT and one complete Content-Range | PASS |
| Anonymous reads | Origin-bearing JSON alt=media and path-style GET return exact bytes; JSON CORS response permits the origin | PASS |
| Ruby anonymous workflow | Explicit endpoint, initialized bucket, create_file, bucket.file lookup/bytes and delete/missing lookup | PASS |

These are raw HTTP browser substitutes; they do not establish browser CORS enforcement. The permissive profile deliberately permits anonymous operations. XML bucket CORS policy and strict IAM are covered elsewhere; no new claims are made here.

The changed browser authority is sent through Go's `Request.Host` (`localhost` with the same port), while the guarded transport continues dialing the configured loopback origin. This exercises the server's Host handling without allowing outbound requests or depending on DNS aliases. It does not simulate container DNS or port forwarding. The server-created Location keeps the configured API origin; no new origin-default decision is needed.

`just check-sdk` invokes Ruby with `https.rb --migration-scenario` on the permissive endpoint before the existing full Ruby suites. The existing standalone Ruby invocation remains unchanged. The temporary buckets file is supplied only to the permissive server; strict fixture setup is unaffected. There is no shared component assigned to this slice by the onboarding plan.

## Sources and boundaries

Reviewed on 2026-10-25: [resumable upload guide](https://docs.cloud.google.com/storage/docs/performing-resumable-uploads), [CORS behavior](https://docs.cloud.google.com/storage/docs/cross-origin), and [Ruby anonymous endpoint API](https://docs.cloud.google.com/ruby/docs/reference/google-cloud-storage/latest/Google-Cloud-Storage). The existing JSON v1 Discovery baseline remains revision `20261013`, SHA-256 `1f74b7a93b3d2872cc332fc20dafae1657fc255930ce336618bea3d0b4d95ae0`. Existing SPEC-CORS-EDGES, SPEC-SIGNED-BROWSER-CORS and resumable/precondition gaps remain open: local Host rewriting and permissive anonymous success do not resolve provider parity. SDK pins remain those in the committed Go and Ruby lockfiles.

Parallel uploads belong to `migrate-upload-admission`, with scenario integration in `migrate-guide-compose`. Project-less buckets.list and HTTP healthcheck defaults belong to `migrate-defaults`. Compose Location guidance belongs to `migrate-guide-compose`. None is exercised or changed here.

## Failed probes and handoff

| Case | Failure | Fixing slice |
| --- | --- | --- |
| Scoped gcs-mimic cases | None; all cases pass and no failed or skipped migration test remains | None |

The excluded parallel-upload and project-less-list cases retain their existing assigned slices above. No new unassigned gcs-mimic failure was discovered.

## Source comparison

One Go scenario execution against `fsouza/fake-gcs-server:1.56.1` (image digest `sha256:797ce226d62f947c009dc40246b30cfb456b8473d8241407f9d6f2c04e4d69ef`) failed the strict assertions: both copy replays returned success instead of 412, preflight used `Access-Control-Allow-Origin: *` (media preflight also omitted Allow-Methods), and PUT after DELETE 499 returned 200 instead of 410; the Ruby scenario execution failed `migration bucket.file metadata differs`.

The source comparison used HTTP, temporary empty `assets`/`exports` seed directories, `-external-url` set to the loopback published origin and `-public-host` set to its `localhost` authority. The image was pulled and run, never built. Successful initiation/default upload/Attrs/read/copy were reached before the Go replay assertion failed; browser transfer/read assertions after failed preflight and Ruby download assertions after failed metadata were not reached. A wildcard CORS response is a source formatting difference, not by itself proof of a browser failure. Source success is therefore not claimed, and source behavior does not override the Google-oriented negative tests. Later migration documentation must retain this limitation rather than claim full replacement.

## Verification

Verified on 2026-10-25 with Rust 1.99.1, Go 1.27.3, Storage Go v1.70.0/API v0.308.0, Ruby 4.0.9, Bundler 4.0.23 and Storage Ruby 1.62.1. Scenario source is commit `b5ff104`. The committed manifests and lockfiles did not change. Freshness check `cargo xtask dependencies check` passed with every input CURRENT.

| Command | Result |
| --- | --- |
| `GCS_MIMIC_GO_TEST_RUN=TestMigrationAppScenario bash scripts/sdk-go.sh` | PASS; default/single-request Go workflow, media/resumable browser wire flow and cancellation |
| `bundle exec ruby https.rb --migration-scenario` from `sdk-tests/ruby` against an isolated initialized HTTP server | PASS; anonymous lookup/create/read/delete |
| `just check-sdk` | PASS once on final source; Go vet/full suite, Ruby origin self-check, migration case, permissive and strict full suites |
| `just check` | PASS once on final source; fmt, clippy, full Rust tests, ledger/generated views and shell/release harness checks |
| `npx --yes markdownlint-cli2 docs/en/slices/migrate-app-scenario.md` | PASS |

DESIGN-082, DESIGN-083 and UX-003 remain PARTIAL. UX-007 retains its existing IMPLEMENTED scoped status; these additional wire cases do not claim actual-browser or full migration validation. No container builds, benchmarks, real-cloud requests or dependency additions were performed. HTTPS transport, real browser enforcement and Compose replacement were not newly exercised by this slice.
