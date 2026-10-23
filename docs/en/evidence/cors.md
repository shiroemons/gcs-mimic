# Basic APIs, CORS and HTTPS verification

<!-- markdownlint-configure-file {"MD013": false} -->

Verified on **2026-10-23** for delivery orders 13–15 and the
[frozen CORS contract](../slices/cors.md). The local CORS behavior rows
BEHAVIOR-BUCKET-009, DESIGN-051, DESIGN-052 and DESIGN-053, and browser journey
UX-007 meet their declared acceptance. Broad API/SDK rows retain their remaining
cases and PARTIAL status. No signed URL, general XML write, real Google service,
iOS device or full Google-method compatibility is claimed.

## Runtime and metadata

The [final Rust transcript](cors-checks.txt) records **591 passing workspace tests**,
zero failures/skips, one additional focused parser test, formatting and workspace/all-target Clippy with warnings
denied. It includes the existing basic API, condition/checksum/permission,
pagination, streaming, concurrent mutation, process-crash and recovery suites.
Commands:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --offline -- -D warnings
cargo test --workspace --offline
```

The final tests include:

| Case | Behavior |
| --- | --- |
| `json_cors_preflight_defaults_never_authenticate_or_grant_access` | Six advertised methods, requested headers/max age, missing-method 404, malformed header denial, all supported JSON namespaces and denied actual mutation after preflight. |
| `json_cors_wraps_bucket_object_streaming_errors_and_validators` | JSON bucket/object CRUD, media, errors, 304/204, exposed common headers, no-Origin/internal-route boundary and Vary. |
| `xml_cors_uses_persisted_bucket_rules_without_changing_json_or_permissions` | Strict update denial, stale condition preservation, XML GET/HEAD/preflight allow/deny/header/credentials, clearing and independent JSON access. |
| `mobile_origins_and_default_ports_round_trip_and_authorize_cors_independently` | Persist/restart capacitor/ionic/custom/null rules; exact custom scheme/port boundaries, HTTP/HTTPS default ports, XML subdomains and invalid Origin denial. |
| `bucket_cors_metadata_round_trip_conditions_replacement_and_restart` | Create/GET/PATCH/PUT, preservation/replacement/null/empty clearing and restart in native and legacy profiles. |
| `bucket_cors_malformed_input_preserves_entire_resource` | Invalid rule/list/string/token/age input preserves all bucket metadata. |
| `bucket_cors_concurrent_conditioned_updates_have_one_winner` | Eight conditioned mutations yield one winner. |
| `bucket_cors_migrates_version_eleven_and_rejects_invalid_library_updates` | Transactional schema 11→12, preserved old bucket, durable rules, invalid library updates and excessive/escaped/many-empty-rule input rejection. |
| `bucket_cors_large_metadata_lists_page_without_loss` | Sixty accepted 20,000-rule configurations, original names-only selector, complete names/full traversal, changed page sizes and restart in both profiles. |
| `bucket_list_metadata_budget_preserves_large_rows_and_cursor_boundaries` | Unexpanded lookahead, small rows around large singleton pages, prefix/count-zero/final/empty cursor boundaries. |
| `json::cors::tests` and `xml::cors::tests` | Custom scheme syntax, opaque `null`, first-rule semantics, case-insensitive headers, wildcard boundaries, credentials and effective ports. |

Stored minified CORS is capped at 64 KiB even through the public catalog API.
A cheap input-size check precedes JSON serialization. Existing wire/header/body,
catalog/concurrency/streaming bounds remain in effect; object bodies are unchanged.
Schema 12 adds one nonnull JSON column transactionally. Supported old catalogs
migrate; an older binary cannot reopen an upgraded catalog. This is a metadata
migration, not a reversible downgrade or a new power-loss guarantee.

The final resource repair changes catalog bucket-list admission and server
continuation handling only. Before decoding a row, the catalog charges a 256 KiB
expanded-metadata page budget; an oversized first row is returned alone to
preserve every accepted rule and ensure progress. Lookahead stays unexpanded,
and continuation names the last returned bucket. This is an admission estimate,
not a total allocator or universal RSS ceiling. The
[contract](../slices/bucket-xml-cors.md#bounded-bucket-list-pages) describes its
singleton exception and parsing/transient limits. The official
[buckets.list contract](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets/list),
checked on 2026-10-23 (updated 2026-10-22), permits short pages. Clients selecting
fields must include `nextPageToken` to traverse them.

## Pinned SDKs and operation boundary

[Go output](cors-sdk-go.txt) records HTTP and HTTPS suites, each with 43 passing
top-level tests and no skips. Storage v1.70.0/API v0.308.0 on Go 1.27.3 cover all
six bucket and nine object methods in the basic milestone, permissions,
conditions, paging/checksums, media/multipart/resumable, copy/rewrite/compose
and persistent CORS configuration. The final CORS tests additionally verify
explicit `:443` configuration against a port-omitted preflight Origin.

```sh
bash scripts/sdk-go.sh
GCS_MIMIC_SDK_SCHEME=https bash scripts/sdk-go.sh
GCS_MIMIC_GO_TEST_RUN='Test(BucketCORS|StrictAuthBucketCORS)$' bash scripts/sdk-go.sh
GCS_MIMIC_SDK_SCHEME=https GCS_MIMIC_GO_TEST_RUN='Test(BucketCORS|StrictAuthBucketCORS)$' bash scripts/sdk-go.sh
```

[Ruby output](cors-sdk-ruby.txt) records Ruby 4.0.9, Bundler 4.0.23 and Storage
1.62.1 across HTTP/HTTPS × permissive/strict, with all four cases passing. Each
uses the [guarded runner](../../../sdk-tests/ruby/https.rb), explicit loopback
endpoint and optional static fixture token. It executes bucket insert/get/list/
patch/update/delete and object insert/get/list/patch/update/delete/copy/rewrite/
compose; verifies derived bytes, CRC32C/SHA-256, three streamed resumable chunks
and the advertised same-origin Location. Generated official methods cover calls
missing from the high-level SDK. Ruby's compose defaults send unsupported nulls;
the tested supported subset specifies `delete_source_objects: false` and
destination metadata, as explained in the [Ruby guide](../../../sdk-tests/ruby/README.md).
No Rails, ADC, metadata server or real-cloud request was used.

The SDK transcripts pin their executed runtime hashes. Full Go and Ruby suites
ran before the final Origin/default-port corrections. The later changes were
confined to CORS parsing/matching and the CORS metadata size cap; ordinary
no-Origin/non-CORS SDK operation behavior and all lockfiles remained unchanged.
Go's CORS cases were rerun on the final matcher under both transports; its
final runtime hash matches the fixed browser runtime. Ruby sends neither Origin
nor bucket CORS settings, so its four earlier cases remain applicable to that
unchanged path. Final-source Rust regressions cover both paths. This comparison
reuses the recorded SDK results without claiming a second full execution.
Those SDK runs also precede the final catalog/server pagination repair. No SDK
client, lockfile, CORS matcher, TLS or mutation path changed for that repair;
final Rust and the constrained container below exercise the affected list path.

## Browser and mobile-origin boundary

The [actual-browser report](cors-browser.md) and [browser output](cors-browser.txt)
record **88 passing checks** in Chromium 156.0.8078.47 on isolated Debian 12.
Strict/permissive initial and restart phases used a fixed prior-source runtime copy whose
SHA-256 remains
`769447146c243564bf9a2e56c4885eb6aa014ffe3262a816ac883f3835814c13`.
Source/input hashes are recorded before and after execution. HTTPS host and peer
origins verify CA and SAN; TLS checks and browser security are enabled.
JSON CRUD/media/multipart/resumable, exposed ETag/Location, independent bucket
rules, XML GET/HEAD/preflight/headers, denied origins/methods/headers/credentials,
strict 403 and persisted rules/bytes pass. Clearing rules affects uncached reads.
That hash identifies the recorded browser execution, before the pagination
repair. The browser was not rerun; unchanged parser/matcher/TLS input identities
and the precise server-only list delta explain reuse in the browser report.

The [Google/Apple/Capacitor research](../slices/cors-mobile-origins.md) distinguishes
app deep links, WebView Origin and API destination. Custom-scheme wire tests
prove local handling, not real iOS emission or Google's acceptance of every
custom/opaque origin. Exact provider exposure lists and undocumented validation
remain in SPEC-CORS-EDGES. JSON credentialed preflight, signed transfers and
cookie identity are outside this scope.

## Native and container journeys

The [prior-source native guide transcript](cors-guide.txt) executes the exact shell blocks
under “Native binary and configuration”, “Browser CORS and mobile application
origins”, and the first built-in “Local HTTPS with an explicit development CA”
block in the runnable guide. Release build, basic native API/metadata/condition
checks, CORS defaults/custom origins and trusted TLS smoke pass.

[Prior-source container smoke output](cors-containers.txt) records builds and
isolated HTTP journeys on Linux ARM64 natively and AMD64 under Docker Desktop
emulation. Both pass host/peer access, probes, non-root/read-only/resource settings,
bucket/object operations, capacitor and default-port CORS, persistence across
stop/start and recreation, seeding/reset and SIGINT/SIGTERM. Only each script's
own disposable volumes are removed.

[Additional HTTPS container output](cors-containers-https.txt) reuses those exact
built images without rebuilding. Both run the new CORS guide shell block and
TLS smoke, then preserve configuration/bytes across restart. Host curl and
digest-pinned peer curl verify the CA, SAN and custom-origin XML HEAD response.
Those earlier image identities are:

| Platform | Local image/index digest |
| --- | --- |
| Linux ARM64 | `sha256:c43627f25b60f568ba7ca05c2695a605574d732b619694b4397bfbcf79ae8842` |
| Linux AMD64 | `sha256:6b7a8d95e0673bdb00a830562ac92b777004b1a7ac64c10b1e16af6cd6aed969` |

Commands use `bash scripts/smoke-containers.sh linux/arm64 linux/amd64`, then
the guide's Compose config/up/stop/start flow with `--no-build`, the matching
local image and explicit host-reachable advertised HTTPS endpoint. Peer requests
use `https://gcs:4443`; host requests use their published localhost port.
No image/package was published. The entire long guide and remote CI were not
rerun; the named affected blocks, scripts and final wire/SDK scope are the
execution boundary.

## Final constrained bucket-list regression

The [final build and resource transcript](cors-memory.txt) records one native
Linux ARM64 Docker build after runtime freeze and one execution of
[`cors-memory-smoke.sh`](../../../scripts/cors-memory-smoke.sh), including its
restart phase. It uses the existing Dockerfile pins, UID/GID 10001, read-only
root, one CPU, 64 PIDs, 1024 file descriptors, memory **134,217,728 bytes** and
the same memory-plus-swap limit, so no extra swap is available.

The runner creates 60 buckets, each with 20,000 empty rules (60,001 minified CORS
bytes, request below 64 KiB). The formerly failing `fields=items(name)` returns
200 before and after restart. Token-selected names and full-resource traversals
each return all 60 exact ordered names in 60 pages, preserving every rule.
Explicit `maxResults=7` repeats all 60; `maxResults=1` and a narrower prefix
return exactly nine names in nine pages. Assertions reject missing/duplicate
names, repeated tokens, missing/truncated rules or changed empty-rule contents.
Health, bucket GET, media upload/download and selected-field object listing
also pass, with read checks repeated after restart. Only the runner's own
container and volume are removed.

The process remains running with `OOMKilled=false`; the deliberate stop exits
zero. Docker archive reads could not expose `memory.current` or `memory.peak`
from the shell-free image, so neither exact cgroup usage/peak nor process RSS is
claimed. Docker stats snapshots were 21.46 MiB initially and 12.04 MiB after
restart; these are instantaneous Docker-reported values, not peak/RSS results.
The earlier macOS process-RSS reproduction is a separate measurement and is
not a Linux cgroup baseline. This sequential regression establishes this accepted
workload under the configured limit, without concurrency, throughput, latency
or general performance claims.

Final ARM64 image/index digest:
`sha256:6d5b9f6b06f883e7da89afdb5d1a2a44f3ea3e7c66f514aee2bd75a58921d729`.
Its embedded release binary SHA-256 is
`c009ab4a1135b1baeca00ad0cf070f15ae493d8bca27faee97593fb3caf9999c`.
The recorded pre/post-smoke all-source hashes precede a test-only strengthening
in `src/json/tests.rs` that explicitly asserts the 200-item full-projection parser
cap. Those historical hashes are retained; the final focused/workspace Rust
execution includes the strengthened test. Production inputs and the image are
unchanged.
The earlier ARM64/AMD64 images above do not identify this final resource build.

## Inputs, comparison and remaining scope

[Dependency output](cors-dependencies.txt) verifies all **35 inputs CURRENT** at
2026-10-23T10:35:56Z, including latest stable Rust 1.99.1, direct crates across
major releases, SDKs and actual Docker input digests. No dependencies or lockfile
resolutions changed. Exact inputs and daily detection remain in the
[dependency snapshot](../../../compatibility/dependency-status.json).
Discovery/protobuf source identities and a corrected pre-existing protobuf
SHA-256 are recorded in the contract and source manifest.

README CORS comparison uses the same named JSON-default plus XML bucket-rule
scope for every product. Pinned fake-gcs-server source uses global wildcard
middleware, fixed allowed headers, credentials and Location exposure; it does
not enforce per-bucket XML rules. Pinned gcp-storage-emulator source stores
configuration without CORS/OPTIONS dispatch. These are source reviews, not new
reference runtime comparisons or whole-product parity claims.

Broader bucket fields, complete projection/ACL/IAM, SDK/Rails/signing, XML writes,
device/cloud behavior, native AMD64, remote CI and performance/power-loss claims
remain outside this delivered scope. The next bounded slice is order 16,
V4 RSA signed download with final-host validation and signer permission checks.
