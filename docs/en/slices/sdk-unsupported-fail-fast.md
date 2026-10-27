# Unsupported SDK calls fail fast

<!-- markdownlint-configure-file {"MD013": false} -->

Scope: DESIGN-082 and DESIGN-083. This changes rejection behavior, not support for the rejected features.

## Contract and acceptance

| Case | Required behavior |
| --- | --- |
| unsupported-fails-fast-json | Recognized unsupported fields/options, including objectRetention, enabled Autoclass, softDeleted=true and includeFoldersAsPrefixes=true, return 400 badRequest before mutation. No-op/default normalization is unchanged. |
| unimplemented-google-route-not-404 | The pinned method/path combinations below return the same 400 badRequest. No resource lookup or mutation occurs. Retention policies and locking now have their own [contract](sdk-retention-policy.md). |
| real-not-found-unchanged | Implemented operations on absent buckets/objects still return 404 notFound. |
| unknown-path-unchanged | Unknown paths remain 404; this is not a catch-all for the Google namespace. |
| xml-unsupported-consistent | Existing XML unsupported rejections use 400 InvalidArgument, including HEAD's bodyless error. |
| error-message-actionable | Messages name the unsupported field/header/route and suggest removing the option or using a supported operation, with an example. |
| go-sdk-fails-fast | Default high-level SDK calls fail within three seconds under a ten-second context, expose googleapi.Error 400 badRequest, and differ from ErrBucketNotExist/ErrObjectNotExist. |
| ruby-sdk-fails-fast | High-level calls with the default retry count raise InvalidArgumentError (ServiceAccount exposes Google::Apis::ClientError directly), with status 400/reason badRequest within three seconds, rather than a not-found error. |
| legacy-profile | Native and LegacyEmulator use the same rejection policy; strict authentication still precedes rejection. No pinned legacy contract requires false 404s for these routes. |

## Sources and local decision

Checked 2026-10-27: [JSON status/errors](https://docs.cloud.google.com/storage/docs/json_api/v1/status-codes) documents 400 badRequest for requests incompatible with current settings; [XML status/errors](https://docs.cloud.google.com/storage/docs/xml-api/reference-status) documents 400 InvalidArgument. Using them for emulator-unsupported semantics is a local choice (SPEC-SDK-UNSUPPORTED), not a claim that Google rejects valid requests this way. Google itself supports these APIs.

[Go Storage v1.70.0 ShouldRetry](https://github.com/googleapis/google-cloud-go/blob/storage/v1.70.0/storage/invoke.go#L239) retries 408, 429 and every 5xx; 400 is not retryable. SDK pins remain Go 1.27.3 / Storage v1.70.0 / API v0.309.0 and Ruby 4.0.9 / Bundler 4.0.23 / Storage 1.62.1. No dependency is added.

Storage v1 [Discovery](https://www.googleapis.com/discovery/v1/apis/storage/v1/rest), revision `20261020`, SHA-256 `eeb0831df501d131c9497534b2c3d130ea40f9fa215195115d011cb64e1a42bc`, fixes the route inventory. The packaged method extract is revision `20261013` and has the same relevant method/path inventory. Paths are matched before decoding; an encoded slash stays inside a resource segment. Only listed methods match.

## Pinned unsupported routes

These 68 method/path combinations are rejected. Variables occupy one nonempty raw path segment, including percent-encoded slashes. Metadata-only objects.insert is included; media/multipart/resumable upload routes remain implemented. Atomic objects.move is now covered by the [move slice](sdk-object-move.md); retention locking by the [retention slice](sdk-retention-policy.md).

| Method | Path |
| --- | --- |
| `POST` | `/storage/v1/b/{bucket}/anywhereCaches/{anywhereCacheId}/disable` |
| `GET` | `/storage/v1/b/{bucket}/anywhereCaches/{anywhereCacheId}` |
| `POST` | `/storage/v1/b/{bucket}/anywhereCaches` |
| `GET` | `/storage/v1/b/{bucket}/anywhereCaches` |
| `POST` | `/storage/v1/b/{bucket}/anywhereCaches/{anywhereCacheId}/pause` |
| `POST` | `/storage/v1/b/{bucket}/anywhereCaches/{anywhereCacheId}/resume` |
| `PATCH` | `/storage/v1/b/{bucket}/anywhereCaches/{anywhereCacheId}` |
| `DELETE` | `/storage/v1/b/{bucket}/acl/{entity}` |
| `GET` | `/storage/v1/b/{bucket}/acl/{entity}` |
| `POST` | `/storage/v1/b/{bucket}/acl` |
| `GET` | `/storage/v1/b/{bucket}/acl` |
| `PATCH` | `/storage/v1/b/{bucket}/acl/{entity}` |
| `PUT` | `/storage/v1/b/{bucket}/acl/{entity}` |
| `GET` | `/storage/v1/b/{bucket}/storageLayout` |
| `POST` | `/storage/v1/b/{bucket}/relocate` |
| `POST` | `/storage/v1/b/{bucket}/restore` |
| `DELETE` | `/storage/v1/b/{bucket}/defaultObjectAcl/{entity}` |
| `GET` | `/storage/v1/b/{bucket}/defaultObjectAcl/{entity}` |
| `POST` | `/storage/v1/b/{bucket}/defaultObjectAcl` |
| `GET` | `/storage/v1/b/{bucket}/defaultObjectAcl` |
| `PATCH` | `/storage/v1/b/{bucket}/defaultObjectAcl/{entity}` |
| `PUT` | `/storage/v1/b/{bucket}/defaultObjectAcl/{entity}` |
| `DELETE` | `/storage/v1/b/{bucket}/folders/{folder}` |
| `POST` | `/storage/v1/b/{bucket}/folders/{folder}/deleteRecursive` |
| `GET` | `/storage/v1/b/{bucket}/folders/{folder}` |
| `POST` | `/storage/v1/b/{bucket}/folders` |
| `GET` | `/storage/v1/b/{bucket}/folders` |
| `POST` | `/storage/v1/b/{bucket}/folders/{sourceFolder}/renameTo/folders/{destinationFolder}` |
| `DELETE` | `/storage/v1/b/{bucket}/managedFolders/{managedFolder}` |
| `GET` | `/storage/v1/b/{bucket}/managedFolders/{managedFolder}` |
| `GET` | `/storage/v1/b/{bucket}/managedFolders/{managedFolder}/iam` |
| `POST` | `/storage/v1/b/{bucket}/managedFolders` |
| `GET` | `/storage/v1/b/{bucket}/managedFolders` |
| `PUT` | `/storage/v1/b/{bucket}/managedFolders/{managedFolder}/iam` |
| `GET` | `/storage/v1/b/{bucket}/managedFolders/{managedFolder}/iam/testPermissions` |
| `PATCH` | `/storage/v1/b/{bucket}/managedFolders/{managedFolder}` |
| `DELETE` | `/storage/v1/b/{bucket}/notificationConfigs/{notification}` |
| `GET` | `/storage/v1/b/{bucket}/notificationConfigs/{notification}` |
| `POST` | `/storage/v1/b/{bucket}/notificationConfigs` |
| `GET` | `/storage/v1/b/{bucket}/notificationConfigs` |
| `DELETE` | `/storage/v1/b/{bucket}/o/{object}/acl/{entity}` |
| `GET` | `/storage/v1/b/{bucket}/o/{object}/acl/{entity}` |
| `POST` | `/storage/v1/b/{bucket}/o/{object}/acl` |
| `GET` | `/storage/v1/b/{bucket}/o/{object}/acl` |
| `PATCH` | `/storage/v1/b/{bucket}/o/{object}/acl/{entity}` |
| `PUT` | `/storage/v1/b/{bucket}/o/{object}/acl/{entity}` |
| `POST` | `/storage/v1/b/{bucket}/o/bulkRestore` |
| `GET` | `/storage/v1/b/{bucket}/o/{object}/iam` |
| `POST` | `/storage/v1/b/{bucket}/o` |
| `POST` | `/storage/v1/b/{bucket}/o/{object}/restore` |
| `PUT` | `/storage/v1/b/{bucket}/o/{object}/iam` |
| `GET` | `/storage/v1/b/{bucket}/o/{object}/iam/testPermissions` |
| `GET` | `/storage/v1/b/{bucket}/o/{object}/viewFullContext` |
| `POST` | `/storage/v1/b/{bucket}/operations/{operationId}/advanceRelocateBucket` |
| `POST` | `/storage/v1/b/{bucket}/operations/{operationId}/cancel` |
| `GET` | `/storage/v1/b/{bucket}/operations/{operationId}` |
| `GET` | `/storage/v1/b/{bucket}/operations` |
| `POST` | `/storage/v1/projects/{projectId}/hmacKeys` |
| `DELETE` | `/storage/v1/projects/{projectId}/hmacKeys/{accessId}` |
| `GET` | `/storage/v1/projects/{projectId}/hmacKeys/{accessId}` |
| `GET` | `/storage/v1/projects/{projectId}/hmacKeys` |
| `PUT` | `/storage/v1/projects/{projectId}/hmacKeys/{accessId}` |
| `GET` | `/storage/v1/projects/{projectId}/serviceAccount` |
| `POST` | `/storage/v1/b/{bucket}/rapidCaches/{rapidCacheId}/disable` |
| `GET` | `/storage/v1/b/{bucket}/rapidCaches/{rapidCacheId}` |
| `POST` | `/storage/v1/b/{bucket}/rapidCaches` |
| `GET` | `/storage/v1/b/{bucket}/rapidCaches` |
| `PATCH` | `/storage/v1/b/{bucket}/rapidCaches/{rapidCacheId}` |

## Verification

Acceptance was recorded before implementation. On the unchanged server, Go retention creation, softDeleted and includeFoldersAsPrefixes each exhausted a ten-second context after retrying 501; the nine other new calls returned ambiguous 404s. Ruby's new high-level unsupported call raised UnimplementedError after default retry handling. No negative case was removed or skipped.

On 2026-10-27:

- `cargo check --workspace --all-targets --locked --offline`: PASS before tests.
- `cargo test --locked --offline --lib json::`: PASS, 57 tests.
- `cargo test --locked --offline --test unsupported_http --test bucket_http unsupported_`: PASS, 5 tests, including all 70 pinned unsupported method/path combinations, actionable messages, no rejected bucket creation, retained 404s and both compatibility profiles. Invalid strict credentials still fail authentication first.
- `cargo test --locked --offline --lib server::unsupported`: PASS, pinned method/path classification and opaque object-name boundaries.
- `cargo test --locked --offline --test auth_http virtual_host`: PASS, 18 tests, including an explicit virtual-host XML object named storage/v1/b/x/acl. Selected XML objects take priority over Google route rejection.
- `cargo test --locked --offline --test auth_http auth_http_strict_rejects_anonymous_and_unsupported_credentials`: PASS; metadata-only insert is now a recognized rejection, invalid credentials remain rejected, and unknown routes retain 404.
- `cargo test --locked --offline --test auth_http xml::xml_unsupported_operations_fail_before_reading_a_body`: PASS, XML 400 InvalidArgument before body consumption.
- `BUNDLE_FROZEN=true bundle check`: PASS with the pinned Ruby/Bundler on PATH.
- `just check-sdk`: PASS over HTTP before the regional rebase. Go vet and the full Go suite pass, including 12 unsupported calls with ten-second contexts and three-second fail-fast assertions. Ruby self-check, migration, permissive/strict suites and V4 signatures pass, including all 13 unsupported calls with default retries and explicit status/reason assertions. ServiceAccount's generated ClientError is checked separately from high-level InvalidArgumentError.
- `cargo xtask dependencies check`: PASS; all 39 pinned inputs CURRENT. No new dependency or configuration key.
- `RUST_TEST_THREADS=8 just check`: PASS on the rebased sources, 1012 Rust tests with no failures or ignored tests, formatting, native/cross-target clippy, ledger/generated views and offline script checks. An earlier default-parallel run hit EOF in the existing anonymous bucket-permission test (`read_until` returned 0 before complete headers); that test passed unchanged on its own and in this complete run. The transient cause was not established.
- Markdownlint: PASS for the changed documents. The existing compatibility policy has 23 pre-existing long lines; only MD013 was disabled for that file's check, without changing repository lint configuration or reformatting unrelated paragraphs.

The optional Japanese textlint check is not clean: the plan has existing findings and flags the required term スライス and feature-ID cells; the new Japanese companion has two false-positive Jira-ID warnings for DESIGN-082/DESIGN-083. These stable ledger IDs and the owner's requested plan vocabulary are retained. This is not a claim that Japanese textlint passes.

After rebasing onto the regional-location implementation (`f171458`), both SDK test blocks and both gap records are preserved, and the feature matrices are regenerated. Location successes/400 invalidArgument remain distinct from unsupported placement's 400 badRequest. Integrated checks:

- `cargo test --locked --offline --test bucket_http bucket_regional_locations_round_trip_and_rejections_preserve_state`: PASS.
- `GCS_MIMIC_GO_TEST_RUN='TestBucketLocations|TestUnsupportedFailsFast' bash scripts/sdk-go.sh`: PASS, Go vet and both high-level SDK tests, including all 12 fail-fast calls.
- `bundle exec ruby sdk-tests/ruby/https.rb` with the isolated HTTP endpoint: PASS, including ruby-regional-location and ruby-unsupported-fails-fast together. The full SDK suite was not repeated after rebase because the JSON/XML routing implementation had no rebase conflict; the merged Ruby tests were checked separately.

Broader SDK/ACL/policy behavior remains PARTIAL. The rejection policy is deliberately emulator-specific, and the gap stays OPEN. HTTPS-mode SDK, container and guide runs, benchmarks and real-cloud calls were omitted as requested. The reference comparison script's PATCH/PUT rejection expectations were updated and `bash -n scripts/compare-reference-emulator.sh` passed, but the comparison was not rerun. The ordinary HTTP SDK suite retains its existing isolated TLS trust unit test; no HTTPS emulator run was performed.
