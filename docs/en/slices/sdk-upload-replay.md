# SDK upload replay and repeated upload headers

<!-- markdownlint-configure-file {"MD013": false} -->

Contract checked: **2026-10-29**. This is a scoped addition to JSON-OBJECTS-007,
BEHAVIOR-OBJECT-008, BEHAVIOR-OBJECT-009 and DESIGN-082. The feature and gap
ledgers remain authoritative; this slice does not complete the broader methods
or close SPEC-013 and SPEC-RSM-EDGES.

## Official contract and fixed inputs

The [JSON parameters reference](https://docs.cloud.google.com/storage/docs/json_api/v1/parameters#xgoog-gcs-idempotency-token)
describes token-based duplicate prevention and response replay. Its recommendation
to retry within a minute is not an exact minimum or maximum retention guarantee.
The [data-validation reference](https://docs.cloud.google.com/storage/docs/data-validation#json-api-resumable-uploads)
defines a final resumable checksum over the whole object. It does not establish
the provider's behavior for an identical whole-object declaration on earlier
chunks. Repeated advisory MIME headers outside initiation are also an explicit
local SDK compatibility rule, rather than a provider-exactness claim.

Sources fetched on 2026-10-29:

| Source | Revision or SHA-256 |
| --- | --- |
| JSON parameters HTML | `e69eaee58e41dc796eb6033af8f1f97e2fcc9d85efa92e28458c798ea441933f` |
| Data-validation HTML | `2ddb1135c8114f850a8daa0501714f9ef63ec2c834f252991915ef88f7670f91` |
| [Storage JSON Discovery](https://storage.googleapis.com/$discovery/rest?version=v1) | revision `20261020`; `eeb0831df501d131c9497534b2c3d130ea40f9fa215195115d011cb64e1a42bc` |

SDK inputs remain the committed scorecard lockfiles: Node.js Storage 8.4.0,
PHP Storage 2.6.1 / Core 1.74.0 / Guzzle 8.3.1, and Python Storage 3.17.1.
The official dependency checker reported all inputs CURRENT at slice start;
no dependency or SDK request header is removed or replaced.

## Successful single-request uploads

Media and multipart upload accept one nonempty invocation token with or without
generation conditions. Empty or duplicate token headers still return 400
`invalidParameter` before reading the body. Missing-token requests retain their
existing behavior. Copy has the separately bounded scope below. XML, resumable
initiation, PATCH, DELETE, rewrite, compose and other token-bearing operations
are outside this replay implementation.

The token's SHA-256 identifies a bounded catalog entry. The request binding
contains the exact upload URI (including fields/projection and query spelling),
advertised API base, supplied encryption-key digest, local authentication profile
and principal ID. It also binds the bucket incarnation observed before body input.
Different principals, buckets, request bindings or incarnations cannot obtain
another invocation's response. The original raw token and encryption key are
not persisted. Principal grants are not part of the key: current permissions
are re-evaluated when the request and publication run.

Each body still streams through the existing two-slot bridge, size and EOF
checks, MD5/CRC32C validation, file synchronization and immutable publication.
An incremental SHA-256 over actual media bytes distinguishes conflicting bodies
independently of MD5/CRC32C. The completion fingerprint also includes the selected
bucket/name, content type, metadata and generation/metageneration conditions.
Its versioned local representation can reject otherwise equivalent requests
after an implementation change; it must never turn that mismatch into a second
mutation. Multipart framing/boundaries are not object content.

Advisory conditions are bypassed only for an existing matching retained token.
Publication rechecks authorization, bucket incarnation and replay inside its
immediate transaction, before enforcing ordinary mutation conditions. Sequential
or concurrent identical retries return the original generation/resource, even
with `ifGenerationMatch=0`, after a later overwrite/deletion, or after restart.
A new token remains a new operation and must meet all original conditions.
An expired invocation also becomes a new operation with ordinary conditions.

Successful publication and its full ObjectRecord/ACL snapshot commit together.
The response is rendered deterministically from that snapshot using the bound
base/projection/fields. Full projection never reloads mutable object ACLs on a
replay. Only ACLs visible to the original identity at publication are retained
for rendering; current ACL-read permission is required before exposing them.
Loss of ACL-read permission omits ACL/owner fields while retaining the original
authorized metadata response. Current upload permission is still required.
Failed requests are not cached. A failed checksum or conflicting token returns
an error without publishing or replacing the original object.

Schema 35 adds `upload_replays` with an indexed deadline. The local retention
period is exactly ten minutes from successful publication, without renewal.
At most 4096 successful invocations and 64 KiB per completion are retained.
Expiry is enforced on lookup and reclaimed by subsequent token publications;
there is no replay maintenance polling task. Full capacity rejects a new token
with 503 `backendError`; retained duplicates still replay. Oversized completion
snapshots reject publication. Expired records may remain physically present
until the next token operation, while count and per-record size stay bounded.
Startup validates schema, capacity and completion decoding one row at a time.
The serialized completion bound is 256 MiB on disk; no completion is evicted
early to admit new work or silently weaken the ten-minute replay guarantee.

SQLite commit and filesystem publication retain their existing crash protocol.
A duplicate's redundant blob is reclaimed by reference lookup; a successful
commit's blob is preserved even when SQL commit outcome is ambiguous. Response
loss and restart do not erase the committed replay. This is not a claim about
every filesystem crash window, host power loss or Google's exact cache policy.

## JSON copy replay

Copy uses the same ten-minute, 4096-entry, 64 KiB completion catalog without
eviction. Its binding includes the exact method/URI, raw metadata-body digest,
advertised API base, source and destination encryption-key digests,
authentication profile/principal and both bucket incarnations. A changed
query, body, key or principal is a conflicting invocation, not a new copy.
The retained response is selected before resolving a mutable source generation.
Thus a successful response can replay after source/destination overwrite or
deletion and after restart, without recreating the deleted destination.

Current source-read and destination-create permissions remain mandatory on
every retry, including inside publication. ACL visibility uses the retained
snapshot and current ACL-read permission. Fresh operations still enforce
source/destination conditions, hashes, holds, retention and overwrite rights.
Concurrent identical copies commit one generation and completion together;
redundant derived blobs enter the existing recovery/deletion journal. A full
catalog or oversized completion fails before destination publication commits.
This scoped local replay rule does not establish exact provider cache behavior.
Token admission checks destination conditions/capacity before resolving the
source; a fresh token can therefore return 412/503 before a source 404/412.
Non-token error ordering is unchanged and provider precedence is unverified.
Bucket recreation during an in-flight copy returns 412 before publication.

## Python advisory content type

Python's ordinary multipart request and later resumable PUT can repeat
`X-Upload-Content-Type`. Validate a single nonempty value as a MIME type, then
treat it as advisory outside initiation. It does not replace multipart metadata
or the media part's MIME, and never changes the persisted resumable MIME or total.
Initiation retains its existing metadata-over-advisory MIME selection and
`X-Upload-Content-Length` enforcement. Duplicate/malformed declarations fail;
unsupported length declarations outside initiation remain rejected.

## PHP whole-object chunk checksums

The pinned PHP SDK repeats the complete object's MD5/CRC32C through
`requestOptions.restOptions.headers` on all chunks. This is distinct from a
per-chunk checksum. Nonfinal requests validate syntax and consistency against
the session's existing declarations, then persist those declarations with the
durable offset in the same checkpoint. An interrupted chunk cannot acknowledge
its data or introduce a new declaration. Repeated chunks may repeat the same
declaration; conflicts fail without changing the acknowledged prefix.
Nonfinal status-only requests may repeat an already-persisted declaration or
subset after syntax/consistency validation, without changing hashes or offsets.
New or conflicting declarations fail without invalidating the usable session.
Completed status, final-status and data retries can repeat the persisted
declarations or a subset, but cannot introduce a new algorithm, even with a
correct checksum. Conflicts return 400 without invalidating the retained
completion or changing the object; a later identical retry still replays.

Finalization rereads the complete staged object in bounded buffers and verifies
all persisted MD5/CRC32C declarations, including when the final request omits
them. A mismatch cannot publish and preserves the old generation, metadata and
bytes. Nonfinal `X-Goog-Meta-*` remains unsupported. Provider parity for hashes
on nonfinal chunks remains unverified; SDK compatibility does not close that gap.

## Acceptance coverage

| Behavior | Test |
| --- | --- |
| Conditioned token retry, malformed token rejection | `obj_http_upload_tokens_with_generation_match_allow_one_create_and_overwrite`, `obj_http_upload_tokens_reject_empty_or_duplicate_headers_before_body`, corresponding `obj_multipart_http_upload_tokens_*` tests |
| Unconditional media replay, later overwrite, collision, checksum, restart | `upload_replay_unconditional_response_survives_overwrite_and_restart` |
| Concurrent retry, one generation and no extra blobs | `upload_replay_concurrent_requests_publish_one_generation` |
| Lost response, restart and bucket recreation | `upload_replay_lost_response_and_recreated_bucket_do_not_duplicate_mutations` |
| Unconditional multipart, MIME precedence and restart | `obj_multipart_http_unconditional_replay_and_python_advisory_mime` |
| Capacity, exact local expiry and caller authorization | `upload_replay_capacity_expiry_and_current_permissions` |
| Full projection, fields and later ACL/object changes | `upload_replay_full_projection_retains_acl_after_mutation_and_deletion` |
| ACL permission revocation | `upload_replay_full_projection_rechecks_revoked_acl_permission` |
| Authentication-mode change and a principal named permissive | `upload_replay_never_crosses_authentication_profiles_after_restart` |
| Python initiation and repeated PUT MIME | `python_upload_content_type_is_advisory_after_initiation` |
| Empty, malformed and duplicate advisory MIME; preserved empty initiation fallback | `python_advisory_content_type_rejects_empty_malformed_and_duplicate_values` |
| PHP repeated/conflicting declarations, restart, bad whole MD5/CRC32C | `php_chunk_hashes_persist_and_validate_the_whole_object` |
| Completed status/final/data checksum retries preserve the completion | `completed_retries_reject_new_checksums_without_invalidating_completion` |
| Copy response loss, changes, deletion, restart, concurrency and permissions | `copy_token_*`, `concurrent_copy_token_retries_publish_one_generation_without_extra_blobs` |
| Copy source/destination key binding and full cache | `copy_replay_binds_both_encryption_keys_and_keeps_retained_entries_at_capacity` |
| Oversized copy completion rolls back destination and generation | `copy_replay_oversized_completion_rolls_back_destination_and_generation` |

Container builds and the full SDK scorecard run belong to integration. Neither
is inferred from these native tests. Parent ledger rows retain unrelated pending
cases, existing evidence and their current statuses.

## Native verification

The following historical slice checks preceded the final integration source.
Final-source verification and the complete SDK result are collected in
[SDK integration](sdk-scorecard-integration.md).
Executed on 2026-10-29 with the committed Rust 1.99.1 toolchain and lockfile:

| Command | Result |
| --- | --- |
| `cargo check --workspace --all-targets --locked` | PASS |
| `cargo test --workspace --locked` | PASS; 1239 passed, 0 failed, 0 ignored across 52 result summaries, including schema migration/read-only/recovery, import, seed, strict authorization and existing upload tests |
| `cargo test --locked --test object_http --test object_multipart_http` | PASS; 35 media/object HTTP tests and 22 multipart HTTP tests |
| `cargo test --locked --test object_http --test object_multipart_http replay -- --nocapture` | PASS; 5 final replay tests, including unconditional concurrent media/multipart and full-projection restart |
| `cargo test --locked --lib upload_replay -- --nocapture` | PASS; 5 SDK/replay tests, including exact local expiry and nonfinal status checksum rejection |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | PASS |
| `cargo fmt --all --check` | PASS |
| `npx --yes markdownlint-cli2 docs/en/slices/sdk-upload-replay.md` | PASS; 0 issues |
| `cargo xtask dependencies check` | PASS; all checked inputs CURRENT; no new dependencies |

The targeted commands cover the final replay/header acceptance assertions;
the workspace run additionally exercises the broader schema and adapter callers.
These checks contact only local test listeners. Dependency/source verification
is a separate explicit online action; no real-cloud data operation was run.
