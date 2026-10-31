# XML multipart uploads

<!-- markdownlint-configure-file {"MD013": false} -->

Contract checked on 2026-10-30 for EXT-XML-004, EXT-XML-011, EXT-XML-013,
EXT-XML-019, EXT-XML-020 and EXT-XML-023. The feature ledger owns implementation
status. This is a scoped local implementation, with explicit resource limits;
it does not claim complete Google wire parity or infrastructure durability.
Completion XML grammar was refreshed on **2026-10-31** using the shared bounded
quick-xml reader described in [XML bucket operations](xml-bucket-operations.md).

## Sources

The following official pages were retrieved on 2026-10-30. Digests identify the
retrieved HTML; Discovery and protobuf do not define the XML request grammar.

| Contract | Official source | SHA-256 |
| --- | --- | --- |
| Initiation metadata | [Initiate a multipart upload](https://docs.cloud.google.com/storage/docs/xml-api/post-object-multipart) | `412b68c74535c7f275ea0e0219d5d390e7369588a51b9845947bb424c87a6b9f` |
| Part upload and integrity | [Object upload part](https://docs.cloud.google.com/storage/docs/xml-api/put-object-multipart) | `8161c2f7af5c2bdb9c253277724b7d5e30492a18af64eb774abb9764857614eb` |
| Ordered assembly and errors | [Complete a multipart upload](https://docs.cloud.google.com/storage/docs/xml-api/post-object-complete) | `ca7a37bf66411db9852a43f9e157e38968917f6901e3999284debd99085b2202` |
| Cancellation | [Abort a multipart upload](https://docs.cloud.google.com/storage/docs/xml-api/delete-multipart) | `3ba51cdf7ec84bf2f01bf88604452748ab53d6249a376ebfdaa76d9f8ca60481` |
| Part pagination | [List object parts](https://docs.cloud.google.com/storage/docs/xml-api/get-object-multipart) | `ee4cca833dfa44d29430d4026124e152a7b7bf4529470cfa96436495fa6f701b` |
| Upload pagination | [List multipart uploads](https://docs.cloud.google.com/storage/docs/xml-api/get-bucket-uploads) | `17869ec2359971fe870014d70b7f23be46f40dc8182d71579f3de99c660289c3` |
| Size and count limits | [Quotas and limits](https://docs.cloud.google.com/storage/quotas) | `a189581300a0a518d2871c55148f2a94e5e866c348a31501274424206f215143` |
| Operation permissions | [IAM permissions for XML requests](https://docs.cloud.google.com/storage/docs/access-control/iam-xml) | `3b0e26919c69eb14e8df992df995c8b61a7039cacacfdcb6a767f37f03c8cdfb` |

The comparison baseline remains fake-gcs-server v1.56.1, commit
`896ece51e49eed0946089f3089f6343855c0d106`. Its behavior does not establish
Google compatibility. No real-cloud request is part of this verification.
The completion reference was retrieved again on 2026-10-31; its updated digest
above replaces the previous snapshot for that operation only.

## Request and response acceptance

Path-style and configured XML virtual-host selection reuse existing routing,
authentication and CORS. JSON and local management namespaces retain their
existing boundaries. Upload IDs are opaque random identifiers, not credentials;
each request requires its current operation permission in strict mode.

| Operation | Accepted request | Observable result |
| --- | --- | --- |
| Initiate | `POST /BUCKET/OBJECT?uploads`, empty body; supported ordinary XML upload metadata, class and CSEK headers | XML `InitiateMultipartUploadResult` with Bucket, Key and UploadId; no ordinary object is created |
| Upload part | `PUT /BUCKET/OBJECT?uploadId=ID&partNumber=N`; bounded media body, optional Content-MD5 or x-goog-hash | Empty 200 with the part ETag; replacing N leaves only its newly committed bytes selected |
| List parts | `GET /BUCKET/OBJECT?uploadId=ID`, optional max-parts and part-number-marker | Ordered Part records containing number, ETag, size and modification time; at most 1000 entries and continuation only when truncated |
| List uploads | `GET /BUCKET?uploads`, optional prefix, delimiter, max-uploads, key-marker and upload-id-marker | Name/ID-ordered active uploads and deduplicated CommonPrefixes; combined page limit and continuation after the whole prefix group |
| Complete | `POST /BUCKET/OBJECT?uploadId=ID` with CompleteMultipartUpload/Part/PartNumber/ETag XML | One new generation with ordered selected bytes, initiation metadata, completion-time timestamps, CRC32C and an opaque ETag |
| Abort | `DELETE /BUCKET/OBJECT?uploadId=ID`, empty body | Empty 204; the session becomes unavailable and parts are journaled for reclamation |

Part numbers range from 1 through 10000 and need not be contiguous. Completion
requires a nonempty, strictly increasing manifest. Missing parts or mismatched
ETags return 400 InvalidPart; duplicate or descending numbers return 400
InvalidPartOrder. Every selected part except the last must contain at least
5 MiB; smaller nonfinal parts return 400 InvalidArgument without publication.
Parts may contain at most 5 GiB. The configured object cap bounds both retained
part bytes per session and the assembled object, and never exceeds 5 TiB.

Completion removes every part, including uploaded parts omitted from the
manifest. Unknown, completed, aborted or locally expired IDs return 404
NoSuchUpload for an authorized existing bucket. Completion replay does not
allocate another generation. Ordinary JSON/XML uploads and object listing never
expose unfinished parts. The unchanged provider policy rejects multipart
generation/metageneration conditions rather than pretending to enforce them.

Completed multipart objects retain an internal MD5 for integrity checks, but
their external metadata has no MD5 hash. They have no fabricated componentCount.
The [MD5 availability contract](object-md5-availability.md) defines persistence,
opaque ETags and adapter rendering. Existing XML conditional-read limitations
remain; JSON conditional reads use their existing validators.

## Authorization and interactions

Initiation, part writes and completion require storage.multipartUploads.create
and storage.objects.create. Completion also rechecks overwrite-delete permission
and destination holds/retention inside shared publication. List requests use
storage.multipartUploads.list or storage.multipartUploads.listParts; abort uses
storage.multipartUploads.abort. Possession of an upload ID grants no permission.
The current policy and observed bucket project are rechecked in the operation's
catalog transaction. Sessions bind a durable bucket incarnation, so deletion and
recreation cannot retarget them.

CSEK declarations are parsed and validated through the existing key layer.
Part writes and completion must repeat the initiation key; missing or changed
keys fail before mutation. Final object reads retain existing key requirements.
This inherits the existing local key-fingerprint simulation, not an encryption
service or a KMS integration. Initiation metadata is applied once at publication;
metadata supplied to part or completion requests is rejected explicitly.

V4 continues signing the original request. V2 canonical resource construction
now binds uploads, uploadId and partNumber in addition to versions, ordered by
subresource name. Adding, removing or changing an operation/part/upload selector
invalidates the signature. The existing path-style-only V2 guard is preserved.

## Storage, failure and resource acceptance

Schema 37 adds session, part and deferred-staging-deletion tables after the
schema 36 MD5 marker. Logical names never become filesystem paths. A part uses a
new opaque staging file, a bounded channel and incremental MD5/CRC32C. Confirmed
transport EOF, flush, file sync and staging-directory sync precede its short
catalog checkpoint. A failed replacement preserves the previously committed
part. Reference checks distinguish an ambiguous commit from an uncommitted file.

Completion queries selected part metadata, then reads one file at a time with a
64 KiB buffer, rechecking its hashes while building a durable staged output.
No part payload or complete object is collected in memory. A fully hashed
ephemeral derived transfer hands the output to existing publication, generation,
ACL/default-hold, retention, notification and change-journal logic. Session
removal and part-deletion journaling commit in that same transaction. Failures
preserve the prior destination unless publication already committed; a response
timeout or an error after commit can have an unknown client outcome.

Committed completion/abort/part replacement survives failed cleanup. The error
is logged, health records it, and its journal remains for maintenance and startup
recovery. Reclamation processes at most 32 staged IDs per catalog job. Recovery
drains retired parts, validates retained regular-file lengths and removes
unowned staging through existing scans. The injected clock and existing
next-deadline scheduler perform expiry without idle polling.

Owned activity and writer leases cover staging, body transfer, completion and
cleanup after caller cancellation. Local instance reset returns busy while those
leases exist, journals all multipart staging IDs and clears all three tables
when idle. An already opened ordinary immutable reader remains independent.

## Deliberate local limits

- At most 128 sessions are retained globally. Sessions expire seven days after initiation; Google permits unlimited idle duration and concurrent sessions.
- One mutating operation owns a session at a time. A concurrent part write, completion or abort receives retryable 503 instead of waiting in an unbounded queue. This safely serializes overwrite/abort/complete races; simultaneous part writes are not Google-parity behavior.
- The retained part-byte budget equals max_object_bytes per session. A replacement and completion may each temporarily need an additional body-sized staged file. These disk bounds do not guarantee free space or a particular process RSS.
- Completion documents are capped at 2 MiB, part ETags at 128 bytes, and part count at 10000. The pull parser retains only the current scalar and selected part metadata, without building a document tree. The 64 KiB/256-element creation-document limits do not apply. XML 1.0 declarations, legal whitespace/quotes, recognized default or prefixed namespaces, standard/numeric references, CDATA and comments are supported. DTDs, external/custom entities, invalid declarations, unknown namespaces, duplicate fields/attributes, extra/multiple roots and trailing content are rejected. Namespace attributes are capped at 16 per element and 128 bindings in scope; the schema fixes nesting to three levels. Unknown provider XML extension behavior is not inferred.
- Listed maxima must be positive uint32 values and are capped at 1000. Names and echoed listing filters that XML 1.0 cannot represent are rejected explicitly. Exact provider error wording/order, multipart object ETag encoding, owner fields, lifecycle AbortIncompleteMultipartUpload execution, billing, archival transitions, cross-platform power-loss semantics and broader optional headers remain outside this scope.

These limits and the existing local storage/CSEK/auth boundaries keep the broad
feature rows PARTIAL. They must not be labeled EXACT merely because the local
acceptance tests pass.

## Verification

The native wire corpus is `tests/xml_multipart_http.rs`. It covers sparse ordered
assembly, overwrite of a part, part and upload pagination, delimiter continuation,
metadata/CRC-only responses, restart, checksum and malformed-operation rejection,
strict permissions and forbidden overwrites, CSEK, holds, bucket recreation and
local reset. Storage tests under `src/storage/catalog/xml_multipart/tests.rs`
cover EOF, exclusive leases, admission/clock expiry, failed write/flush/sync,
checkpoint/publication commit, ambiguous commit and crash recovery. Snapshot and
Unix PowerLoss cases cover part replacement, assembly, publication, abort and
reclamation seams. The model does not undo every filesystem unlink or model
unseamed new-file entries, so it is not evidence of real host-power-loss safety.

The original slice added no dependency. Its completion-parser correction uses
the approved quick-xml 0.42.0 dependency and provenance recorded in the
[bucket contract](xml-bucket-operations.md#pinned-sources).
The release-scope freshness check on 2026-10-30
reported Rust 1.99.1 and all checked release inputs CURRENT; SDK ecosystems and
development-only container inputs were explicitly skipped by that scope.
The following commands passed on 2026-10-30:

| Command | Result |
| --- | --- |
| `cargo check --tests` | Passed with unfiltered compiler output |
| `cargo test --lib multipart` | 19 passed, including eight XML storage tests, 34 crash/power-loss snapshots, parser and V2 selector binding, and existing JSON multipart/CSEK regression tests |
| `cargo test --test xml_multipart_http --test xml_storage_http --test object_multipart_http --test admin_http` | 60 passed: 7 XML multipart, 13 XML LIST/COPY, 22 ordinary JSON multipart and 18 administration cases |
| Dedicated `rustfmt --check` and `git diff --check` | Passed |
| Markdown lint for this contract and the reset contract | Zero issues |
| `cargo xtask dependencies check --scope release` | Passed; all checked release inputs CURRENT |

`cargo xtask lint` found no multipart violation after the completion validation
helper was separated. Its latest repository-wide run remained blocked by the
concurrently edited administrative publication function's line count; final
integration owns that remaining check and whole-repository formatting.
Official SDK/container/browser and real-cloud verification remain integration
work. No performance claim is made.

The completion-grammar refresh on 2026-10-31 passed the following targeted checks:

| Command | Result |
| --- | --- |
| `cargo check --locked --test xml_multipart_http --test xml_bucket_http` | Passed, including the final namespace-reference cleanup |
| `cargo test --locked --test xml_multipart_http` | 8 passed, including the new malformed-document and one-byte chunked completion case |
| `cargo test --locked --test xml_bucket_http` | 13 passed after shared-reader reuse |
| `cargo test --locked --lib xml::` | 20 passed, including declaration/entities/namespaces, 10000/10001 parts and 2 MiB/+1-byte boundaries |

`multipart_manifest_rejects_malformed_xml_without_publication_and_accepts_chunked_standard_xml`
verifies that malformed declarations, root-prefix confusion, multiple roots,
trailing content, duplicate declaration attributes, unknown namespaces, DTDs and
external entities preserve both the previous object bytes and the uploaded part.
It then completes the same session with a valid declaration, prefixed namespace,
standard/numeric references and one-byte HTTP chunks, checking exact final bytes.
`manifest_keeps_ten_thousand_parts_and_two_megabyte_limits` preserves the original
manifest/count limits independently of the creation-document tree limits.
Targeted strict Clippy passed for the API/parser candidate. The final
namespace-reference cleanup passed type checking, but the unit/production-lint
rerun could not compile the concurrently edited
`src/storage/catalog/upload_replay.rs` because of E0282/E0283 inference errors.
Final shared-tree production lint remains an integration gate.
