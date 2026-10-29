# SDK compatibility integration scope

<!-- markdownlint-configure-file {"MD013": false} -->

Contract and native verification date: **2026-10-29**. The feature ledger remains
authoritative. This integration prepares a final five-SDK comparison; no new
SDK score or complete-method compatibility is claimed here.

## Acceptance scope

| Area and related feature IDs | Required observable behavior | Contract and tests |
| --- | --- | --- |
| JSON-OBJECTS-007, BEHAVIOR-OBJECT-008/009, DESIGN-082 | Media/multipart token retries return one original committed resource, with or without conditions, across concurrency, overwrite/deletion and restart; conflicting bodies, permissions, checksums and bucket incarnations cannot bypass publication checks | [Upload replay](sdk-upload-replay.md); `upload_replay_*` and media/multipart HTTP tests |
| Same upload scope | Authentication mode and principal ID are separate replay inputs; a strict principal named `permissive` cannot retrieve a permissive-mode completion after reopening the same catalog | `upload_replay_never_crosses_authentication_profiles_after_restart` |
| JSON-OBJECTS-007, BEHAVIOR-OBJECT-005 | Repeated valid advisory MIME does not replace metadata MIME; empty, malformed and duplicate values fail on media/multipart and subsequent resumable PUT; initiation retains its existing empty-header fallback | `python_upload_content_type_is_advisory_after_initiation`, `python_advisory_content_type_rejects_empty_malformed_and_duplicate_values` |
| JSON-OBJECTS-007, BEHAVIOR-OBJECT-011/014 | PHP whole-object MD5/CRC32C declarations persist with chunk checkpoints, reject conflicts and validate complete bytes before final publication, including after restart | `php_chunk_hashes_persist_and_validate_the_whole_object`; [local checksum boundary](sdk-upload-replay.md#php-whole-object-chunk-checksums) |
| JSON-BUCKETS-008/013, JSON-OBJECTS-010/015, BEHAVIOR-COMMON-005, DESIGN-018/020 | Supplied resource ACL arrays replace or clear atomically with metadata; omitted arrays persist; invalid values, stale conditions, UBLA/PAP and permission failures roll back the whole mutation | [ACL contract](json-acl.md); `tests/acl_http.rs` |
| Same ACL scope | Update/getIamPolicy without setIamPolicy allows reordered identical echoes but rejects changes without metadata mutation; update/setIamPolicy without getIamPolicy can replace ACLs without receiving them in the response, through PATCH and PUT | `object_resource_acl_echo_and_replacement_keep_read_and_write_permissions_separate` |
| DESIGN-085/086, AUDIT-004 and related query routes | Query booleans accept `true`/`false` and Python's `True`/`False`; other spellings, numbers and whitespace fail, and JSON body boolean typing remains unchanged | `json::tests` boolean cases and bucket/ACL HTTP tests |
| DESIGN-082/083/085/086, AUDIT-004 | All comparison products share HTTPS, a trusted temporary CA and unchanged signed URLs; every client rejects wrong CA/SAN and external origins, including Ruby's actual SDK Faraday connection | [Transport contract](sdk-scorecard-tls.md); `bash sdk-scorecard/test-tls.sh` |

The official sources, checked content digests, SDK revisions and exact local
limits are in the linked contracts and committed lockfiles. Source documents
and registries were fetched explicitly; no Google Cloud data operation was run.
The complete dependency checker reported all inputs CURRENT, including Rust
1.99.1 and the existing SDK/container pins. No dependency was added.

## Native and transport verification

The integrated source passed the initial unfiltered
`cargo check --workspace --all-targets --locked` before testing.

| Command | Result |
| --- | --- |
| `cargo test --locked --test acl_http --test bucket_http --test object_http --test object_multipart_http --test object_metadata_http` | PASS; 142 tests, no failures or ignored tests |
| `cargo test --locked --lib object_resource_acl_echo_and_replacement_keep_read_and_write_permissions_separate -- --nocapture` | PASS; one real-HTTP permissions regression |
| `cargo test --locked --lib upload_replay -- --nocapture` | PASS; six replay/header tests before the additional advisory MIME negative case |
| `cargo test --locked --lib python_advisory_content_type_rejects_empty_malformed_and_duplicate_values -- --nocapture` | PASS; media/multipart/continuation rejection and preserved initiation fallback |
| `bash sdk-scorecard/test-tls.sh` | PASS; 16 real-client CA/SAN/origin combinations, including the Ruby SDK adapter |
| `bash scripts/with-test-tmpdir.sh cargo test --workspace --all-targets --locked` | PASS; 1247 passed, 0 failed, 0 ignored across 50 test-result summaries |
| `cargo fmt --all --check` | PASS |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | PASS |
| `go test ./...` in `sdk-scorecard/go` | PASS; SDK transport and TLS frontend packages |
| `node --test sdk-scorecard/node/local-http.test.mjs` | PASS; real loopback SDK transport and external-socket rejection |
| `bash sdk-scorecard/test-score.sh` | PASS; five-SDK/historical weighting and invalid evidence rejection |
| `cargo xtask compatibility validate`, `render`, `check` | PASS; 606 feature rows, 401 parameter locations and 357 schema fields |
| Markdown lint for the nine updated contracts/README documents | PASS; 0 issues |

The final workspace test uses the repository's temporary macOS RAM-disk wrapper
and retains real file synchronization. It includes the last advisory-header fix.
No performance or physical-disk/power-loss claim follows from that test profile.
Historical ledger verifications retain their original test names and artifacts;
renamed current tests link separately to this final workspace result. The
canonical English and Japanese matrices were regenerated without status changes.
The older media contract's long tables use the same MD013 exception as the other
slice contracts; its content and table syntax still pass Markdown lint.
Full SDK comparison and container builds are deferred to the final validation stage.

## Remaining limits

DESIGN-085, DESIGN-086 and AUDIT-004 remain `PARTIAL`, with their historical
failed HTTP evidence preserved. The new complete five-SDK result is pending;
even all 51 cases passing would establish only this declared permissive,
default-region comparison scope. The common TLS frontend does not verify each
emulator's native TLS listener. Strict SDK authorization, other profiles,
optional transports and real-GCS differential behavior remain pending.

SPEC-013 remains open for other methods and exact provider cache/error rules.
Repeated nonfinal whole-object checksum and advisory MIME acceptance are named
local SDK accommodations. ACL entries still do not authorize requests, and
provider ACL edge parity remains incomplete. Existing retention, security,
bounded streaming and atomic-condition guarantees are retained.
