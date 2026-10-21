# JSON bucket CRUD slice

Contract checked on **2026-10-21**. This document decomposes the adapter subset before
implementation; it does not change the implementation-status ledger. The affected rows are
JSON-BUCKETS-001/002/005/006, BEHAVIOR-COMMON-001/007/013 and BEHAVIOR-BUCKET-001.
Catalog atomicity, persistence, HTTP integration and official Go SDK evidence are now integrated.

## Sources and provenance

All sources below were checked on 2026-10-21:

- [Insert](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets/insert)
- [Get](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets/get)
- [List](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets/list)
- [Delete](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets/delete)
- [Bucket resource](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets)
- [Bucket naming](https://docs.cloud.google.com/storage/docs/buckets#naming)
- [Errors](https://docs.cloud.google.com/storage/docs/json_api/v1/status-codes)
- [Common parameters](https://docs.cloud.google.com/storage/docs/json_api/v1/parameters)
- [Preconditions](https://docs.cloud.google.com/storage/docs/request-preconditions)
- [Locations](https://docs.cloud.google.com/storage/docs/locations)
- [Labels](https://docs.cloud.google.com/storage/docs/tags-and-labels)
- [Label limits](https://docs.cloud.google.com/resource-manager/docs/labels-overview)
- [Soft delete defaults](https://docs.cloud.google.com/storage/docs/soft-delete)
- [Storage classes](https://docs.cloud.google.com/storage/docs/storage-classes)
- [Creation defaults](https://docs.cloud.google.com/storage/docs/creating-buckets)
- [Discovery v1](https://storage.googleapis.com/$discovery/rest?version=v1), revision
  `20261013`, SHA-256 `1f74b7a93b3d2872cc332fc20dafae1657fc255930ce336618bea3d0b4d95ae0`.

Existing gaps SPEC-001/002/003/004/006/007 were reviewed; none establishes additional bucket
CRUD semantics. No real-cloud calls are authorized or needed for these unit tests.

## Explicit subset and deviations

Authentication and authorization are out of scope: no permissive/strict authentication profile,
IAM permission enforcement, ACL ownership, billing, quota or domain-ownership verification is
claimed. Production requires `storage.buckets.create/get/list/delete` respectively. The adapter
does not construct public object URLs or change permissions.

`noAcl` and `full` are accepted. Both return every modeled field; `full` omits
`acl`, `defaultObjectAcl` and `owner` because ACLs and ownership are not implemented.
This is a known deviation, explicitly selected for the official high-level Go SDK, whose
bucket Attrs and list methods request `full`. No ACL or permission fidelity is claimed.
All recognized unsupported request semantics are rejected, even for empty objects/arrays or
false/null values, unless explicitly documented below. These 501 responses are emulator
deviations, not claimed Google errors. Unknown body fields are ignored for forward compatibility;
the checked bucket sources do not establish exact unknown-field behavior, so this remains an
unverified local rule, distinct from recognizing and rejecting unsupported fields.

The local project has no registry: a nonempty project identifier is stored verbatim (at most
256 UTF-8 bytes); a decimal unsigned 64-bit identifier becomes its normalized decimal
`projectNumber`; other identifiers (including decimal overflow) yield string `"0"`.
Whitespace and control characters in identifiers are rejected. This is a local simulation, not project
resolution. Global uniqueness is limited to one catalog.

Location support initially comprises uppercase `US`, `EU`, `ASIA`, with `multi-region` derived
from the location. Other locations return 501, including syntactically plausible regions; no
geographical behavior is claimed. Lowercase location normalization is not inferred from examples:
the checked bucket contract does not explicitly specify case folding, so lowercase is rejected
as unsupported. Storage classes are the Discovery values `STANDARD`, `NEARLINE`, `COLDLINE`,
`ARCHIVE`, `MULTI_REGIONAL`, `DURABLE_REDUCED_AVAILABILITY`; `REGIONAL` and `RAPID` return
501 because their required region/zone locations are outside this subset. This slice records
bucket defaults only and implements no object placement or class transitions.

Soft delete, retention, lifecycle, versioning, default ACLs and organization policy defaults are
not simulated. In particular, Google's default soft-delete retention is not applied: deleting
an empty local bucket removes it immediately. Responses omit these unsupported configurations.
ETags are opaque deterministic local identifiers, not Google's encoding. Dotted-name ownership
verification and the full undocumented set of close misspellings of `google` are not reproduced.

## Method and parameter decisions

Paths below are relative to the externally advertised endpoint. Every query component is decoded
exactly once as UTF-8, with `+` interpreted as space. Malformed escapes/UTF-8, duplicate decoded
parameter names, empty required values and invalid scalar types return 400 `invalidParameter`.
Unknown query names return 400 `invalidParameter`; parameters for other methods are not silently
accepted. The local raw query cap is 16 KiB. Non-insert methods require an empty body.

| Method | HTTP path | Success | Request body |
| --- | --- | --- | --- |
| insert | `POST /storage/v1/b` | 200 bucket resource | JSON object; `name` required |
| get | `GET /storage/v1/b/{bucket}` | 200 bucket resource | None |
| list | `GET /storage/v1/b` | 200 bucket list | None |
| delete | `DELETE /storage/v1/b/{bucket}` | 204 empty | None |

Every method-specific Discovery query parameter is listed here (`bucket` is a path parameter):

| Methods | Parameter | Slice decision |
| --- | --- | --- |
| insert, list | `project` | Supported, required, nonempty; local registry rule above |
| insert, get, list | `projection` | Default `noAcl`; `full` returns the modeled subset without ACL/owner; other values 400 |
| insert | `predefinedAcl`, `predefinedDefaultObjectAcl` | Rejected 501 |
| insert | `enableObjectRetention` | Rejected 501, including false |
| insert, get, list, delete | `userProject` | Rejected 501: billing is not implemented |
| get, delete | `ifMetagenerationMatch`, `ifMetagenerationNotMatch` | Supported signed int64; both may be present; conditions must both hold |
| get | `generation` | Rejected 501: no soft-deleted bucket versions |
| get, list | `softDeleted` | False supported (live buckets); true rejected 501; invalid boolean 400 |
| list | `maxResults` | Supported uint32 including zero; default/max 1000 for noAcl, 200 for full; larger values clamped |
| list | `prefix` | Supported literal UTF-8 prefix; empty means all; local cap 222 bytes |
| list | `pageToken` | Supported opaque cursor, validated and bound to project/prefix; empty means the first page as sent by the pinned Go SDK |
| list | `returnPartialSuccess` | False supported; true rejected 501: no regional failure model |

Common Discovery query parameters apply to all four methods:

| Parameter | Slice decision |
| --- | --- |
| `alt` | `json` supported/default; `media` rejected 501; other values 400 `invalidAltValue` |
| `fields` | Rejected 501: no field selection |
| `key`, `oauth_token` | Rejected 501: no authentication behavior promised |
| `prettyPrint` | False supported (compact JSON); true rejected 501; invalid boolean 400 |
| `quotaUser` | Rejected 501: quota simulation absent |
| `userIp` | Rejected 501: deprecated quota semantics are not implemented |
| `uploadType` | Rejected 501: bucket methods do not upload media |

The common-parameter documentation also describes `callback`; it is rejected 501 (no JSONP).
Nothing is ignored merely because it is inconvenient to implement.
Responses default to compact JSON locally; Discovery defaults to pretty printing. This whitespace
difference is a known deviation. Explicit `prettyPrint=false` is supported for official SDKs.

## Insert body and resource fields

Supported request fields: `name` (required string), `location` (default `US`), `storageClass`
(default `STANDARD`) and `labels` (default empty map). Explicit null is invalid for these fields.
Labels have at most 64 entries, keys of 1–63 Unicode scalar values and values of 0–63;
ASCII characters are lowercase letters, digits, `_`, `-`; keys start with a lowercase letter
or a non-ASCII alphabetic character. Non-ASCII alphabetic/numeric characters are supported;
Unicode combining marks and other ambiguous international characters are a documented subset
limit (400 `invalidArgument`). Invalid types/values return 400 `invalidArgument`; malformed
JSON returns 400 `parseError`; absent required fields return 400 `required`.

Every remaining top-level field in the checked Discovery Bucket schema is recognized and
rejected 501: `acl`, `autoclass`, `billing`, `cors`, `customPlacementConfig`,
`defaultEventBasedHold`, `defaultObjectAcl`, `encryption`, `etag`, `generation`, `hardDeleteTime`,
`hierarchicalNamespace`, `iamConfiguration`, `id`, `ipFilter`, `kind`, `lifecycle`, `locationType`,
`logging`, `metageneration`, `objectRetention`, `owner`, `projectNumber`, `retentionPolicy`, `rpo`,
`satisfiesPZI`, `satisfiesPZS`, `selfLink`, `softDeletePolicy`, `softDeleteTime`, `timeCreated`,
`updated`, `versioning`, `website`. Output-only fields are never used to override server state.

Emitted bucket resource fields: `kind="storage#bucket"`, `id=name`, `name`, `selfLink`,
`projectNumber`, `metageneration`, `timeCreated`, `updated`, `etag`, `location`, `locationType`,
`storageClass`, and `labels` only when nonempty. All int64/uint64 fields are JSON strings.
`selfLink` uses the fixed `api_base_url` configuration and percent-encodes the bucket segment.
The default is `http://127.0.0.1:4443`; CLI `--api-base-url` overrides environment
`GCS_MIMIC_API_BASE_URL`, JSON `api_base_url`, then the default. It accepts an absolute
HTTP(S) base with an optional path, without credentials/query/fragment. Neither Host nor
forwarded headers influence links. Set it explicitly for non-default ports or proxies;
one fixed endpoint cannot simultaneously advertise host and Compose DNS origins.
Creation uses metageneration 1 and equal creation/update timestamps, truncated to milliseconds.
Times use UTC RFC 3339 with exactly three fractional digits; dates outside years 0000–9999
return a 503 instead of panicking. ETags include creation time and metageneration so recreating
at a different instant changes identity. Same-name recreation in the same millisecond cannot
be distinguished with the shared record; no stronger incarnation guarantee is claimed.

List emits `kind="storage#buckets"`; omits `items` when empty and `nextPageToken` when absent.
No `unreachable` field is emitted because partial regional success is unsupported. The catalog
must supply sorted, prefix-filtered, project-filtered rows strictly after the cursor and bounded
by the effective page size. It must use a lookahead to determine continuation, without reading
the entire catalog. For `maxResults=0`, return no items and no token (local rule; upstream's
zero behavior is not described). Cursors are versioned hex envelopes with a corruption checksum,
not credentials or authenticated tokens; clients must treat them as opaque. They carry the last
name, project and prefix, not an offset or snapshot. New names behind the cursor are skipped.

## Error decisions

All errors use JSON content type `application/json; charset=UTF-8` and the envelope
`error.code`, `error.message`, and a one-element `error.errors` array with `domain="global"`,
`reason` and matching `message`. Error messages do not reveal catalog-internal diagnostics.

| Case | Status | Reason |
| --- | --- | --- |
| Invalid query/path escape, invalid query scalar/token/name | 400 | `invalidParameter` |
| Missing required project or name | 400 | `required` |
| Malformed JSON | 400 | `parseError` |
| Invalid body fields/labels | 400 | `invalidArgument` |
| Body stream read failure | 400 | `badRequest` |
| Method mismatch on a recognized route | 405 with `Allow` | `methodNotAllowed` |
| Unsupported recognized semantics | 501 | `notImplemented` (local deviation) |
| Existing name (`CatalogError::AlreadyExists`) | 409 | `conflict` |
| Missing bucket (`NotFound`) | 404 | `notFound` |
| Nonempty bucket (`NotEmpty`) | 409 | `conflict` (local reason choice; delete page specifies failure without exact reason) |
| Failed catalog condition (`PreconditionFailed`) | 412 | `conditionNotMet` |
| GET not-match failure after match succeeds | 304, empty body | No JSON envelope |
| Body on get/list/delete | 400 | `invalidArgument` (rejected from headers without reading) |
| Catalog unavailable (`Unavailable`) | 503 | `backendError` |
| Caller-supplied body cap exceeded | 413 | `uploadTooLarge` (metadata cap is a local deviation) |

The insert metadata cap is 64 KiB. The body collector never copies a frame that would cross the cap and never accumulates beyond
it. An already allocated transport frame is owned by Hyper; this helper cannot bound a custom
Body producer's allocations. Caller/server admission, body timeout and cap remain integration
responsibilities, enforced by the connection deadline and the existing connection admission cap.

## Acceptance cases

These IDs describe observable assertions; catalog cases remain integration obligations.

| ID | Assertion | Evidence owner |
| --- | --- | --- |
| BKT-ROUTE-001 | Exact collection/resource paths and four verbs; unrelated/trailing/nested paths yield None | Adapter unit tests |
| BKT-ROUTE-002 | Split raw path before decode; malformed escapes/UTF-8 rejected; encoded slash never creates a nested route | Adapter unit tests |
| BKT-ROUTE-003 | Wrong method returns 405 and correct Allow header | Adapter unit tests |
| BKT-QRY-001 | Every parameter decision above; duplicates including encoded names and unknown/misplaced names fail | Adapter unit tests |
| BKT-QRY-002 | Required project, query limits, booleans, projection and integer overflow/sign validation | Adapter unit tests |
| BKT-INS-001 | Minimal body defaults; labels, project, timestamps and metageneration preserved | Adapter unit tests |
| BKT-INS-002 | Required name, JSON shape, scalar types, labels limits and unsupported fields rejected | Adapter unit tests |
| BKT-INS-003 | Locations/classes accepted only in declared subset; unsupported semantics never succeed | Adapter unit tests |
| BKT-INS-004 | Insert publishes once; duplicate name conflicts globally in local catalog | Integration |
| BKT-GET-001 | Resource rendering and noAcl projection; absent bucket 404 | Adapter rendering; integration lookup |
| BKT-GET-002 | Metageneration conditions parsed without overflow and checked against selected record | Adapter parsing; integration condition checks |
| BKT-LST-001 | Defaults, zero, uint32 boundary, clamping, prefix and empty list omission | Adapter unit tests; integration filtering |
| BKT-LST-002 | Token round trip, corruption, version, scope mismatch, invalid cursor and length cap | Adapter unit tests |
| BKT-LST-003 | Sorted keyset pages; lookahead; insertion behind cursor; no cross-project leakage | Integration |
| BKT-DEL-001 | Empty deletion 204; absent bucket 404; nonempty 409 | Integration; adapter error mapping |
| BKT-DEL-002 | Both metageneration conditions enforced atomically with deletion | Integration |
| BKT-NAME-001 | 3/63/222 boundaries, ASCII alphabet, global start/end and dot components | Adapter unit tests |
| BKT-NAME-002 | IPv4 dotted-decimal form, goog prefix and google/g00gle variants rejected | Adapter unit tests |
| BKT-ERR-001 | Full envelope, content type, status, messages and every CatalogError mapping | Adapter unit tests |
| BKT-BODY-001 | Empty/exact-cap/multiple-frame/oversize/stream-error/trailer body cases | Adapter unit tests |
| BKT-ID-001 | UTC millisecond rendering, pre-epoch/leap/century boundaries, numeric string fields and selfLink | Adapter unit tests |
| BKT-ID-002 | ETag stability and changes with metageneration/creation; nonempty labels omission | Adapter unit tests |

## Source conflicts and unresolved questions

1. The list method page limits `full` to 200 and `noAcl` to 1000; Discovery describes a
   universal 1000/default 1000, minimum 0. The implementation follows the method-specific
   200/1000 bounds. Zero returns no items/token as an explicit local rule.
2. The list parameter description calls pageToken encoded; the response description calls
   nextPageToken the last bucket's name. Discovery calls it a token. This subset follows
   the explicit encoded-token description and preserves the last-name keyset semantics.
3. Preconditions documentation says a failing metageneration-not-match yields 304, while the
   generic error catalog maps failed conditions to 412. GET evaluates conditions against
   the exact record rendered and returns empty 304 for failed not-match, after checking
   match (412 takes precedence if both fail). DELETE evaluates both inside its transaction
   and returns 412 for either failure. DELETE not-match status remains an unverified local
   deviation: the [preconditions table](https://docs.cloud.google.com/storage/docs/request-preconditions)
   says 304 generally; the [delete reference](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets/delete)
   specifies the condition but no status. This is not real-service conformance evidence.
4. Preconditions guidance says buckets lack generation numbers, while current get/list
   references expose generations for soft-deleted buckets. No soft-deleted behavior is
   inferred from that older general statement; explicit generation requests are rejected.
5. Naming prohibits close misspellings without enumerating them. This subset rejects
   `google` with `o`/`0` substitutions and `goog` prefix; other variants and exact dotted
   component boundary behavior need conformance evidence. Empty components are rejected.
6. Exact unknown-field handling, lowercase locations, metadata-body caps and nonempty-delete
   error reason are not fully specified by these sources. Local rules above remain deviations,
   never evidence of complete Google API fidelity.
7. The current storage-class guide describes `RAPID` and its zonal restriction, while the
   checked Discovery storageClass description lists only the seven older classes. This is
   not evidence of absence: RAPID is recognized and rejected as unsupported, not accepted or
   treated as a typo. REGIONAL is also rejected because this subset has no regional locations.

## Adapter integration API

`json::route(method, uri)` returns `None` for other namespaces, otherwise a parsed
`BucketRequest` or `JsonError`. Request variants carry typed project, conditions, projection,
page size, prefix and decoded last-name cursor. `json::read_body(body, cap)` bounds collection;
`json::parse_insert(bytes, project, now)` creates a `BucketRecord`. The integration layer
executes catalog operations, checks conditions atomically, and calls `bucket_resource` or
`bucket_list` with an advertised base URL. `encode_page_token` creates continuation cursors;
`JsonError::from(CatalogError)` and `JsonError::into_response()` render errors. No function
in this module calls the catalog, reads wall time, binds sockets or performs authentication.

## Integrated verification on 2026-10-21

The local mise shim reintroduces a Rust 1.93.0 override after environment removal, so commands
used `env -u RUSTUP_TOOLCHAIN cargo` with the rustup proxy first in PATH.
The verified compiler was Rust 1.99.1 (`58cf8f9c3`, 2026-10-13).

| Command suffix after the cargo executable above | Result |
| --- | --- |
| `fmt --check` | PASS |
| `clippy --locked --all-targets -- -D warnings` | PASS |
| `test --workspace --locked` | PASS: adapter/config/catalog, six binary HTTP tests, eight catalog tests, eight foundation tests and xtask suites; no skips |
| `xtask dependencies check` | PASS: compiler, all direct dependencies, Action and container inputs CURRENT |

The wire cases are `bkt_http_crud_metadata_conditions_and_errors`,
`bkt_http_query_body_name_and_route_boundaries`, `bkt_http_sorted_scoped_pagination_and_limits`,
`bkt_http_list_clamps_to_1000_with_lookahead`,
`bkt_http_concurrent_duplicates_restart_and_exclusive_directory` and
`bkt_http_failed_recovery_never_reports_ready`. They complement the exhaustive adapter
unit cases; NotEmpty is unreachable without objects and only its error mapping is tested.
`tests/foundation.rs` now supplies isolated catalogs and no longer expects bucket list to
return 404; probes and unrelated routes retain their previous assertions.

`scripts/sdk-go.sh` passes with Go 1.27.3, storage v1.69.0 and API v0.306.0 (HTTP JSON).
The high-level client is unchanged; it exercises full projection and empty initial tokens.
See [integration evidence](../evidence/bucket-crud.md) for exact commands and logs.
Runtime tests use only local endpoints, without Google or metadata-server access.
Process-memory benchmarks, real-cloud differential tests, object/blob recovery, permissions
and fixture seeding remain outside this slice.
