# XML PUT Object verification

<!-- markdownlint-configure-file {"MD013": {"tables": false}} -->

Verified **2026-10-23** on native macOS ARM64, Rust **1.99.1**, edition **2024**.
Code and tests: `9ee0eacead613908063c1c0d514053eed083ec18`.
The [frozen contract](../slices/xml-put-object.md) and `EXT-XML-022` define the
supported subset. The complete official operation remains PARTIAL.

## Executed checks

| Command | Result | Artifact |
| --- | --- | --- |
| `just check` | PASS: 604 Rust tests, including all 13 new XML PUT wire tests; fmt, workspace/all-target clippy with warnings denied, compatibility check and shell checks | [Final raw output](xml-put-object-checks.txt) |
| `cargo xtask dependencies check` | PASS: all 35 inputs CURRENT; no dependencies or lockfiles changed | [Freshness output](xml-put-object-dependencies.txt) |
| `cargo xtask compatibility render`, `validate`, `check` | PASS after the final ledger update; English and Japanese matrices regenerated from the same feature row | [Ledger checks](xml-put-object-ledger.txt) |
| `npx --yes markdownlint-cli2 docs/en/slices/xml-put-object.md docs/en/evidence/xml-put-object.md` | PASS with markdownlint-cli2 0.24.0; a temporary npm cache was used | [Lint output](xml-put-object-ledger.txt) |

The shared checks also validate container-publication shell logic with offline
fixtures and the smoke runner's help/invalid-platform handling. They do not build
or run a container. Raw logs redact the local checkout prefix and otherwise
preserve command output.

## Wire coverage

All test names below are in `tests/auth_http/xml_put.rs`, under `xml_put::`.
Fixtures launch isolated local servers and never contact Google Cloud or a
metadata server.

| Test | Verified behavior |
| --- | --- |
| `xml_put_metadata_hashes_empty_and_encoded_paths_round_trip` | Every supported fixed header, mixed-case and empty custom metadata, combined/repeated hash headers, quoted hexadecimal MD5 ETag, generation/metageneration, full hashes and stored length/encoding; JSON/XML agreement, metadata reset, zero bytes, encoded paths and restart |
| `xml_put_chunked_multibuffer_and_configured_size_boundaries` | 262281 binary bytes in 17003-byte chunks, exceeding the 64 KiB bridge buffer; chunked and Content-Length exact maximum success, known-size and streamed max+1 rejection with unchanged resource, history count and generation counter |
| `xml_put_individual_and_combined_hash_headers_accept_matching_bytes` | Content-MD5 alone, x-goog-hash MD5 alone, CRC32C alone and combined values; automatic 100-continue after acceptance |
| `xml_put_unsupported_inputs_fail_before_continue_and_preserve_state` | ACL, encryption, class, billing, retention/hold, resumable, signing/payload, copy, unknown extensions, ranges, validators, trailers and unsupported queries return final XML 501 without requesting a body |
| `xml_put_invalid_headers_and_checksums_preserve_entire_object` | Malformed/duplicate conditions, hashes, fixed/custom headers, non-ASCII metadata, conflicting MD5, missing length and unknown hash algorithms; reject Content-MD5 hash-list injection, algorithm prefixes and noncanonical padding; MD5/CRC32C mismatch; exact 8 KiB stored metadata boundary and excess |
| `xml_put_strict_permissions_and_public_grants_never_fallback_on_invalid_tokens` | Creator may create but cannot overwrite; viewers/roleless/anonymous callers are denied appropriately; allUsers create versus overwrite grants, registered-only grants, project-plus-bucket permission union, invalid/unsupported credentials and authorized missing bucket |
| `xml_put_conditions_and_concurrent_overwrites_are_atomic` | Generation zero/stale generation/stale metageneration rejection; paired conditions and retry token; two uploads admitted before their bodies yield exactly one 200 and one 412 for matching old generation and for create-if-absent |
| `xml_put_rechecks_create_delete_and_bucket_grants_at_publication` | Creator admitted for an absent name cannot overwrite an intervening create; anonymous permission revoked during streaming is rechecked at publication; old state and generation counter remain intact |
| `xml_put_incomplete_disconnect_shutdown_and_restart_reclaim_artifacts` | Incomplete Content-Length upload yields only empty EOF/reset or XML 400 IncompleteBody; graceful shutdown and forced process termination before EOF preserve bytes/metadata/generation/history; startup recovery removes staged/orphaned files |
| `xml_put_chunked_trailers_and_body_deadlines_cannot_publish` | Unannounced trailers abort, body inactivity and overall request deadline close incomplete streams; all failures preserve previous data and reclaim staging |
| `xml_put_versioning_and_default_event_holds_match_shared_uploads` | Versioning retains selected prior bytes; the current bucket default hold applies, prevents overwrite and survives restart with history |
| `xml_put_bucket_cors_covers_preflight_success_and_denials` | Configured PUT preflight/actual headers, exposed ETag, denied origins/headers and CORS on authorization/unsupported errors without granting write access |
| `xml_put_upload_admission_is_bounded_and_releases_aborted_slots` | Eight admitted incomplete uploads cause the next PUT to return 503 before 100-continue; abort cleanup releases slots without publishing partial objects |

The existing XML unsupported-operation test now targets unsupported PUT `?acl`,
preserving its early-rejection assertion while plain PUT becomes supported.
The shared CORS evaluator already supports PUT; no CORS or JSON semantics needed
implementation changes. The full existing Rust suite remains green.

## Official source pins

The exact fetched bytes below were SHA-256 checked on **2026-10-23**. Discovery
revision **20261013** and the Storage v2 protobuf pin are unchanged; they establish
shared metadata types rather than XML wire behavior.

| Source | SHA-256 |
| --- | --- |
| [PUT Object](https://docs.cloud.google.com/storage/docs/xml-api/put-object-upload) | `177ff787005e083068f1b4882c956e70c37ef92b684dfb7631802d9169fc5645` |
| [XML headers](https://docs.cloud.google.com/storage/docs/xml-api/reference-headers) | `ae196538b0b63d9b95fb871aa433ed8828b4cf2bce76c77d8dd1a232ec904dd0` |
| [XML status](https://docs.cloud.google.com/storage/docs/xml-api/reference-status) | `28f4e699f113cc9817bf4cafa7bdcab5e8cd7119489bf69ed45c06cc301c7b97` |
| [IAM permissions](https://docs.cloud.google.com/storage/docs/access-control/iam-permissions) | `35d86b5458e99876321c8f4c697ba38392fa21cee0b0f0789423a309a57d224a` |
| [Request preconditions](https://docs.cloud.google.com/storage/docs/request-preconditions) | `e2ce1245d5956d9b0b0bd2fd61b8305465fcf166893f74bead2886e44f2fcafd` |
| [Discovery revision 20261013](https://storage.googleapis.com/$discovery/rest?version=v1) | `1f74b7a93b3d2872cc332fc20dafae1657fc255930ce336618bea3d0b4d95ae0` |
| [Storage v2 protobuf at d73dfb3c3be52245625b951b9c0bcf65163c549f](https://github.com/googleapis/googleapis/blob/d73dfb3c3be52245625b951b9c0bcf65163c549f/google/storage/v2/storage.proto) | `a0df6132967b97a89be248f1f24d277ee8df8f8d885db1d4af6249469ff0c1cf` |
| [Go storage/v1.70.0 HTTP client](https://github.com/googleapis/google-cloud-go/blob/storage/v1.70.0/storage/http_client.go) | `a74c0157e327096d641d4a8ca8b544ec6b2e4f8c9e64c463f5d8155e1cec50d0` |

The pinned Go client's `OpenWriter` calls generated JSON `Objects.Insert` with
`Media`. It cannot exercise XML PUT. No new Go check was added; Rust wire tests
directly cover this operation. The existing Go/Ruby suites were not rerun in
this worker; integrated SDK verification belongs to the final combined source.

## Remaining limits

`SPEC-XML-PUT-EDGES` retains exact parser/metadata/error parity questions,
including the local InvalidDigest/BadDigest mapping for CRC32C and the fact that
transport-invalid framing may be rejected by Hyper before XML formatting.
Existing namespace, unknown-input, authentication/permission-order and ETag
gaps remain applicable. Signed PUT, ACLs, encryption, billing, storage-class
override, retention/explicit holds, custom time and other advanced variants are
explicitly unsupported. The ledger lists pending cases rather than treating
their 501 responses as implementation.

Uploads reuse the two-slot bridge with copied chunks no larger than 64 KiB and
the shared eight-upload limit. The tests establish those structural/admission
bounds and cleanup, without claiming a measured RSS/cgroup ceiling, throughput,
or large-object benchmark. No real-cloud differential call, device/browser
execution, container build, guide walkthrough, multi-architecture run or power
loss test was performed.

Before EOF, interruption prevents publication. After confirmed EOF, the existing
filesystem/SQLite publication may finish even after timeout, disconnect or
shutdown, so the outcome can be unknown until reread. This slice does not add
rollback of a running commit or claim every fsync crash boundary is verified.
Next integration: V4 RSA signed PUT headers/payload verification over this
bounded upload path, then the combined browser/container/SDK delivery gate.
