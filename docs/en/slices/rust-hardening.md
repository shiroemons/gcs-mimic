# Internal Rust hardening before 1.0.0

DESIGN-105 collects the internal changes made before the first `1.0.0` release
candidate: module layering, the error model, how catalog work is routed, body
I/O, types and configuration, duplicated rules, lints and tests. Apart from the
decisions under [Behavior decisions](#behavior-decisions), nothing here changes
documented behavior. The [feature ledger](../../../compatibility/features.json)
owns the status; this page is the design. The first slice creates the
verification record, `docs/en/evidence/rust-hardening.md`, and every slice
adds what it ran there.

## Why now

The [1.0.0 gate](../release-policy.md#the-100-gate) does not require internal
refactors. They land before the release candidate anyway, so that the
seven-day soak and the final-source verification run on the code that ships,
and `1.x` patch releases do not carry large internal diffs.

An audit of the source at `4f398afd1` (2026-11-26) found the lint gate and
conventions strict and generic tuning done: no unbounded buffering, no crash-safety
defect and little copied code (jscpd: 0.35 % of production lines). The problems
are structural:

- **The single writer thread does work that does not need it.**
  - XML multipart part commits and completions sync the staging directory
    inside the writer job (`src/storage/catalog/xml_multipart/transfer.rs`),
    which [design](../design.md) forbids; `derived/begin.rs` does it on the
    blocking pool.
  - Reads that change nothing run on the writer: object and bucket listing,
    ACL reads and projections, `has_bucket_permission`, gRPC readable opens,
    resumable and rewrite session lookups, and the notification outbox poll.
- **Requests make more catalog round trips than they need.**
  - `projection=full`, which the pinned Go SDK sends on get, list, patch,
    insert and rewrite, adds a writer job per call; JSON `buckets.list` with
    it costs two jobs per bucket, while gRPC already uses the bulk
    `bucket_acl_projections`.
  - Every writer release wakes the maintenance pass, which then runs about a
    dozen writer jobs even when nothing is due, and wakes itself again.
  - Nine commit sites schedule blob reclamation whether or not anything was
    journaled.
  - Delete reads its bucket row three times and decodes the bucket policy and
    ACL up to four times; XML completion re-reads every part row.
- **Body I/O pays one blocking-pool hop per network frame.** Uploads write each
  frame through `tokio::fs::File`, which copies it and spawns a blocking job
  per call. Re-hashing and assembly loop over tokio reads with 64 KiB stack
  arrays inside async functions (futures of 65,696 and 67,832 bytes).
  Multipart and batch parsing search delimiters byte window by byte window and
  copy the body in 8 KiB pieces. gRPC `ReadObject` still pushes through a task
  and a channel, while HTTP already pulls.
- **Lower layers depend on adapters.**
  - 38 production files outside `src/json` name `crate::json`, including
    storage, auth, import, config and notification.
  - `storage::catalog::admin` builds a bucket by rendering JSON text and
    parsing it back.
  - gRPC and XML reach the JSON adapter through `src/server`, which the current
    `text::json_adapter_dependency` rule cannot see.
- **The shared error type is the JSON adapter's.** `ApiError` carries an HTTP
  status, a plain-text flag and an HTML flag. About 340 functions return it,
  including domain code. gRPC maps it back by matching HTTP status numbers.
  Non-HTTP callers extract `.message`, which is stringification the lint
  cannot see. Display and `source()` conventions drift, so `Report` has to
  remove repeated text.
- **Types carry less than they could.** `ObjectRecord` stores names as
  `String` and generations as `i64` although validated types exist.
  `Config` has 65 public fields with `effective_*` fallbacks. A completed
  resumable session keeps its state and its stored resource in two fields that
  must be set together.
- **The same rule is written in several places.** Default content type, single
  header values, idempotency tokens, calendar math, host normalization, page
  caps, `Expires`, `x-goog-meta-*`, XML precondition headers, and CSEK hash
  visibility each have two to ten copies.

## What stays fixed

These do not change, and each slice proves it:

- the CLI, settings, defaults, help text and diagnostics names;
- the data directory: SQLite schema and column values, the JSON stored in TEXT
  columns, blob layout and replay fingerprints;
- wire responses: HTTP status, headers and body bytes, XML codes, gRPC codes,
  messages and details;
- the seven contract event names and the ready event;
- bounded streaming, durability ordering, authorization and checksums.

Human-oriented diagnostic wording is not contract
([release policy](../release-policy.md)), but slices keep it identical unless
they say otherwise.

Proof, in this order:

1. Golden tests record today's output before the code that produces it moves:
   - error responses for every domain error on all three adapters;
   - stored completion bytes, upload replay documents and fingerprints;
   - the bucket record that the JSON round trip in `admin.rs` produces.
2. Existing tests pass unchanged. A negative test is never removed or
   weakened; a test whose body becomes too small to reach its fault point grows
   its body instead.
3. Every changed durable write path gets the failed-write, failed-flush or
   failed-sync tests and the `Fault::PowerLoss` snapshot and restart tests that
   [AGENTS.md](../../../AGENTS.md) requires, through the seams in
   `src/storage/catalog/fault.rs`.
4. Every slice passes `just check`, which also runs Clippy for every release
   target. Each phase ends with the full test suite and `just check-sdk`.

Measurement follows the project rule: no harness, no repeated samples and no
gate. A slice whose purpose is performance records one short run at local
scale as a fact in the verification record.

## Decisions

### Decided by the owner on 2026-11-26

- **Error boilerplate.** Adopt `thiserror` 2.x as a direct dependency.
  - Checked 2026-11-26: 2.0.21, MIT OR Apache-2.0, Rust 1.77.
  - syn 2 is already built for normal builds, so the edge adds two packages.
  - It is admitted through the [dependency policy](../dependency-policy.md)
    at the start of its slice, after checking the latest release again.
- **Crate structure.** Move the generated protobuf code (about 14,000 lines)
  into its own workspace crate. Enforce every other boundary with layer rules
  inside the single crate. No further split before `1.0.0`; the reasons are
  under [Crate structure](#crate-structure).
- **Measured limits.** A change that raises a memory bound or changes
  SDK-visible pacing is adopted only when one short measured run shows a gain,
  and it updates the documented bound in the same change. This covers:
  - the write batch size;
  - the HTTP/1 connection read buffer;
  - the gRPC read message size;
  - typed serialization of list items.
- **Single-source copies** publish the pinned source blob instead of copying
  its bytes, where the destination bytes equal the source bytes
  ([design](#single-source-copies)).

### Decided by this design

These change no documented behavior:

- **Advisory upload check.** It stays on the writer. Moving it to the read
  pool was tried and rejected in
  [write-path performance](../evidence/write-path-performance.md): integration
  tests require the check to queue behind the writer.
- **Reset and reseed** keep their unlinks and directory syncs inside their
  exclusive writer job, under an explicit allowance. These operations already
  hold every other request back.
- **Delete** runs its expiry purge inside the delete's own immediate
  transaction, with the delete under a savepoint, so a failed precondition
  still commits the purge. Crash states shrink: purge and delete commit
  together or not at all.
- **Bulk ACL projections.** Under a concurrent bucket delete, a listed bucket
  whose ACL projection disappears is returned without ACL fields instead of
  failing the whole list with 404. An authorized object read also keeps its
  successful response without ACL fields if its bucket disappears before projection.
- **In-process derived transfers.** Single-request derived transfers keep their
  offset and hasher in memory instead of rereading the checkpoint.
  - Crash recovery already discards these rows without reading the offset
    (`derived/recovery.rs`), so recovery does not change.
  - The in-process invariant documented in `derived/advance.rs` is restated.
- **Shutdown tracking.** It uses `tokio_util::sync::CancellationToken`, with
  `tokio-util` promoted from the lockfile (it adds no package), plus a small
  task tracker in `src/task.rs`.
  - `TaskTracker` would need the `rt` feature, which adds `futures-util`.
  - Its spawn would also bypass the supervised-spawn rule.
- **JSON responses** stay `serde_json::Value` trees. Only list items may move to
  typed serialization, and only under the measured-limit rule above. Field
  order is pinned by the golden tests either way.
- **Test layout.** Tests move to the repository's majority layout, a
  `#[cfg(test)] mod tests;` declaration with the tests in `foo/tests.rs` (a
  `foo/tests/` directory when large), with an inline module allowed under
  about 200 lines. The 39 `*_tests.rs` files and 3 `*_tests/` directories move
  in one mechanical slice at the end, renaming their ledger test IDs.
  An `xtask` check then refuses new ones.

### Behavior decisions

These change an observable answer. They ship together, with a release note and
ledger updates, in the last `0.x` minor release before the release candidate.

Decided by the owner on 2026-11-26:

- **Which field an input error names.**
  - With `serde_json`'s `preserve_order`, `Map::remove` is `swap_remove`: it
    moves the last key into the removed one's place.
  - So an error naming "the first remaining field" does not follow request
    order (for example `json/derived_metadata.rs`).
  - Inputs use `shift_remove`, and `Map::remove` is disallowed, so errors name
    the first invalid field in request order.
- **XML HEAD with a missing blob.** HEAD opens the blob it never reads.
  - HEAD now answers from metadata, as the JSON metadata GET already does.
  - Only a blob missing at runtime changes the answer, from 503 to 200.
- **XML resumable completion headers.**
  - The completion sends one combined `x-goog-hash: crc32c=..,md5=..` field,
    while every other XML response sends one field per algorithm.
  - It also omits the CSEK response headers.
  - Both follow Google's XML header reference and the pinned SDKs' hash
    parsers. If those do not settle the field format, the completion keeps
    its current format and shares only the ETag rule.
- **Per-part MD5 at XML multipart completion stays.**
  - Completion keeps verifying every part's stored MD5 and CRC32C.
  - AGENTS.md forbids weakening a checksum for speed without a documented
    profile and tradeoff, and the gain is small at local scale.
  - The whole-object CRC32C is computed by combining the part CRCs instead,
    which drops a pass without weakening anything.

Pending the evidence below:

- **Custom time in import preflight and publication.** Preflight accepts year
  `0000` (such as `0000-12-31T23:59:59Z`) and fractions with ten or more
  digits and only zeros after the sixth digit (such as
  `1969-12-31T23:59:59.0000000000Z`). An actual import carrying either
  spelling fails with `Metadata(CustomTime)` during validation of the staged
  metadata, before publication. The candidates are to keep both checks as
  they are, accept both spellings at publication, or reject them at preflight.
  Evidence for the decision comes from the pinned SDK sources, the official
  documentation and the `Object.custom_time` comment in the pinned
  `storage.proto`, under [development](../development.md#no-live-cloud-storage-contact).

- **Negative and zero preconditions on JSON routes.**
  - Today the JSON media, multipart, get and delete routes compare
    `ifGenerationMatch=-1` against the object, so it fails with 412.
  - JSON compose and resumable initiation refuse negative values with 400,
    and so does the XML API (both documented). gRPC compares, which is also
    documented.
  - The evidence comes only from the sources that
    [development](../development.md#no-live-cloud-storage-contact) names:
    - the pinned SDK sources, for whether they validate or forward a negative
      value;
    - the request-preconditions documentation;
    - the storage.proto comments.

    Nothing contacts Cloud Storage.
  - The evidence and a recommendation go to the owner before the behavior
    slice. The candidate is: if no pinned SDK relies on comparison, refuse
    negative values on every JSON object route with compose's wording, through
    one `PreconditionPolicy` shared by JSON and XML. Otherwise keep today's
    behavior and record each route's policy as a local choice.

- **Error precedence for repeated, non-text fields.** `json::resumable::single`
  checks the first value's text before reporting repetition. It reads resumable
  headers and `x-upload-content-type` on JSON media and multipart uploads.
  XML PUT and POST, batch, XML read precondition headers, and
  `upload_headers::content_type` (including JSON uploads' `Content-Type`)
  report repetition first. A JSON upload can therefore use different precedence
  for `Content-Type` and `x-upload-content-type` on the same request. The tests
  `a_non_text_first_resumable_header_wins_over_repetition`,
  `a_repeated_xml_put_header_wins_over_a_non_text_first_value` and
  `a_repeated_batch_header_wins_over_a_non_text_first_value` pin the choice.
  Repetition first is a candidate for a common rule because it rejects an
  ambiguous field without interpreting either value.
- **Custom metadata headers.** JSON resumable completion checks the first
  `x-goog-meta-*` value before reporting repetition, and checks unsupported
  `x-goog-upload-*` fields in the same header order. XML object uploads report
  repeated metadata first. Both accept an empty key from a bare header prefix;
  the XML POST form requires a nonempty field suffix. Using repetition first
  for metadata headers is a candidate, as with the other upload headers.
- **Idempotency token input.** JSON and XML read checks accept one opaque,
  nonblank byte value. JSON replay and XML PUT require visible text as well.
  XML copy refuses the field whenever it is present. The tests
  `json_get_accepts_an_obs_text_token`,
  `json_media_upload_requires_a_text_token`,
  `xml_get_accepts_an_obs_text_token` and `xml_put_requires_a_text_token`
  exercise the four routes with a `0xff` byte. The tests
  `opaque_tokens_admit_bytes_that_text_tokens_refuse`,
  `obj_headers_upload_tokens_require_one_nonempty_value_with_or_without_conditions`,
  `xml_read_idempotency_tokens_require_one_nonempty_value` and
  `xml_copy_failed_conditions_digests_and_unsupported_inputs_preserve_destination`
  cover these boundaries. Requiring visible, nonblank text on routes that
  accept tokens is a candidate. Copy should keep its refusal until it can
  honor the token's replay semantics.
- **Trailing DNS root dot.** Management and trusted-listener host checks
  remove one final dot. Native XML virtual-host selection keeps it. Legacy
  XML removes it only to classify the configured domain, then keeps the raw
  spelling when deciding whether the host selects a bucket. The tests
  `the_request_target_wins_and_malformed_hosts_name_nothing` and
  `legacy_trailing_dot_hosts_do_not_fall_through_to_a_shorter_domain` pin
  the difference. Ignoring one root dot in DNS routing, as the management
  checks do, is a candidate if compatibility checks support it for requests
  that currently use path-style addressing. Signature verification would
  still use the authority as sent.
- **XML precondition header diagnostics.** Object reads and deletes, bucket
  operations and object listings use precondition-specific wording for repeated
  or non-ASCII values. PUT, copy, multipart and resumable initiation use the
  upload header wording. Both sets of routes reject repetitions before
  interpreting the first value and use the same number grammar and range. Using the
  precondition-specific wording on every route is a candidate.
- **JSON bucket metageneration syntax.** Bucket queries and default object ACL
  listing use the same optional `int64` grammar. Retention lock still requires
  `ifMetagenerationMatch` and accepts a leading `+` with its own error wording.
  Accepting `+` consistently is a candidate if pinned clients benefit from it.
- **Anonymous canned ACL wording.** JSON and gRPC name the route's parameter;
  XML says `x-goog-acl on upload`. Uniform wording across the three adapters
  could name the field while keeping their existing error envelope and code.
- **Omitted content types.** JSON media and multipart uploads can store an
  empty type and omit `contentType` from the resource. XML and gRPC uploads
  default these empty fields. Resumable and rewrite apply a default to absent
  declarations, while reads of stored empty types use it in the media header.
  Defaulting every upload at publication is a candidate only if clients can
  tolerate a stored type and a `contentType` field where they are absent today.

Found by the error goldens, which pin them as they are today. Each goes to the
owner with the error slice that touches it:

- **A refused IAM role on JSON reads as a bucket setting.**
  `PolicyInputError::UnsupportedInput` goes through `ApiError::unsupported`,
  which says "does not support bucket API setting role roles/owner; omit this
  setting". The direct gRPC mapping names the IAM role.
- **XML `NotModified` through the shared mapping carries a body.** The mapping
  answers 304 with a 112-byte `InvalidArgument` document. XML reads answer 304
  themselves before it, so this is only reachable if another path maps the
  error.
- **gRPC through `ApiError` turns 409 into `ALREADY_EXISTS`.** For example
  `CatalogError::NotEmpty` is `FAILED_PRECONDITION` through the direct mapping
  and `ALREADY_EXISTS` with another message through `ApiError`. Which gRPC
  paths reach the `ApiError` route is checked in the error slices, which
  remove it.

## Module layers

A module may name modules in its own layer and in lower layers. The three
adapters must not name each other.

| Layer | Modules |
| --- | --- |
| foundation | `base64`, `hex`, `percent`, `decimal`, `clock`, `hashing`, `serde_util`, `error`, `task`, `diagnostics`, `fsutil`, `bounded_body`, `bounded_file`, `time_limit`, `memory_limit`, `fd_limit`, `net`, `match_glob`, `source_tree`, `mime`, `http_conditions`, new `choice`, new `cause`, new `http_header` |
| model | `project`, `acl`, `bucket`, `object`, `metadata_error`, `etag`, `csek`, `auth`, `url`, `notification`, `time_format`, new `profile`, new `input` |
| settings | `config`, `config_input`, `tls` |
| storage | `storage`, `import`, `seed`, `bucket_init` |
| service | `page_token`, `multipart`, new `service` (logic the adapters share, and `ServiceError`) |
| adapters (isolated) | `json`, `xml`, `grpc` |
| transport | `server` |
| application | new `app` (today's binary modules) |

### Rules in `xtask`

`text::json_adapter_dependency` is replaced by two rules:

- `text::adapter_dependency` rejects production code in one adapter that names
  another. Nothing violates it today, so it is zero tolerance from the start.
- `text::layer_dependency` rejects a reference to a higher layer.
  - It is ratcheted per file over today's findings, about 106 sites in 60
    files.
  - It becomes zero tolerance when its last baseline row goes.
  - Keeping it separate from the adapter rule stops a file from trading one
    forbidden edge for another under an unchanged count.

The rules live in `xtask/src/lint/layers.rs`:

- A table names each layer, its modules and whether peers may depend on each
  other.
- References are taken from `crate::m`, `$crate::m`, the top-level members of
  `use crate::{..}`, and `super::` or `self::` chains resolved from the file
  path. A re-export is reported where it is written, so a lower module cannot
  forward an upward dependency.
- The rules skip test files, `#[cfg(test)]` items, comments, strings,
  `src/lib.rs` and the binary's modules.
- The gate fails when a module declared in `src/lib.rs` has no layer, or a
  layer names an undeclared module.
- A test checks that the layer table in [design](../design.md) matches the
  `xtask` table.

The Rules table in [development](../development.md) loses the JSON row and
gains both rules; AGENTS.md's adapter rule is reworded.

### Moves

None of these changes behavior. Re-exports keep the paths that `src/main.rs`
and `tests/` use until the facade slice. Each move fixes the moved code's
upward references in the same change, because the ratchet refuses findings in
new files.

| What | From → to |
| --- | --- |
| `string_enum!`, `InvalidChoice` | `config` → `choice` |
| RFC 3339 parser (`parse_timestamp` and its helpers) | `import::codec` → `time_format` |
| `CompatProfile` | `config` → `profile` |
| `AuthProfile`, `TrustedCaller` | `config` → `auth` |
| `validate_dns_name`; `reserved_path` | `config`; `xml` → `url` |
| `bucket_for_authority`, `request_authority`, `virtual_host_api_authority` | `xml` → `config::domains` |
| `json::validate_project`, `json::validate_bucket_name` | callers use `auth::validate_project_id` and `bucket::name::validate` |
| `Projection` | `json::query` → `acl` |
| `DestinationLocation`, `derived_component_count` | `json::derived_metadata` → `object::derived` |
| `validate_fixed_metadata`, `parse_insert_with_profile` | `json` → `object`, `bucket::resource` |
| Bucket defaults built by rendering JSON (`admin.rs`, `import/filesystem.rs`) | build the record through `bucket::resource` |
| `AdminGrant` | `storage::admin` → `bucket` |
| Range and transcoding negotiation (`range_request`, `Transcoding`, `Gunzip` and their helpers) | `json::objects` → `service::media` |
| JSON_API_V1 notification payload | the outbox receives a renderer at catalog open |
| HMAC lookup in `auth::signature` | an `HmacKeys` trait implemented by `Catalog` |
| Shared request pieces in `server` (access checks, request context, response bodies, `chunk_body`, `object_body`, uploads, copy, the resumable engine, `State`) | `service` |
| gRPC HTTP/2 connection serving | `grpc` → `server::grpc_listener` |
| JSON handlers in `server`; `server::xml_read` | `json::handler`; `xml::read` |

Before moving the shared server pieces, a slice settles where `RequestBody`
and JSON link rebasing in `RequestCtx` go, so that `xml` can import them without
reaching `json`.

### File splits

- `src/server.rs` (2,480 lines) splits into `accept`, `slots`, `connection`,
  `deadline` and `probe` under `src/server/`.
- `src/auth/mod.rs` splits into `permission`, `identity`, `choices`, `error`,
  `registry` and `authenticate`, with its inline tests in `auth/tests.rs`.
  The module path does not change, so its ledger IDs stay.
- `src/fsutil.rs` keeps neutral types and selects per-OS modules
  (`unix`, `linux`, `apple`, `windows`, `unsupported`). `cfg_select!` replaces
  attribute chains where a function body chooses per target.

### Public surface

- **Binary.** The binary's modules (about 4,000 lines) move into the library as
  `app`, and `src/main.rs` keeps only the allocator, the THP call and
  `gcs_mimic::app::main()`.
- **Tests and examples.** A `#[doc(hidden)] pub mod testing` re-exports
  exactly what `tests/` and `examples/` use.
- **Everything else** becomes private, so `unreachable_pub` and `dead_code`
  start reporting. Items only tests use move under `cfg(test)` or go.
- **Misplaced tests.** Integration tests that only call library functions move
  into the library.

### Crate structure

The generated protobuf code moves into a workspace crate that depends only on
`prost` and `tonic`. The slice records one build measurement (a clean build,
and an incremental build after editing a non-protobuf file) as a fact.

The same slice generates protobuf maps as `BTreeMap` (`btree_map` in
`src/grpc/codegen`). Map fields are encoded in `HashMap` order today, so the
bytes of an object with two or more custom metadata entries, stored as a gRPC
completion, and of an `ErrorInfo.metadata` detail change from run to run.
Protobuf leaves map order unspecified and decoding accepts any order, so
nothing that reads them changes; the persisted-format and error goldens then
pin several entries instead of one entry and a re-encoded detail.

A further split waits until after `1.0.0`:

- **Cycles.** The cycles this design removes have to go first anyway.
- **Test seams.** The 673 `cfg(test)` seams, including the fault seam
  referenced from 113 files, cannot cross crates without a feature. That
  feature would likely unify into the binary the integration tests start.
- **Tooling.** The lint gate, baselines, dependency detector and scripts all
  assume one crate.
- **Ledger IDs.** About 500 test IDs would lose their module prefix.

## Error model

- **Neutral errors.** Domain and shared code return typed, protocol-neutral
  errors, and each adapter maps them once with exhaustive matches.
- **Model layer.** `input::InputError { kind, message, field }` carries
  rejected request input. Its message names the rejected value and is the text
  every adapter sends.
- **Service layer.** `service::ServiceError` gathers what shared logic and
  parsers return:
  - `Input`, `Project` and `Auth`;
  - `Catalog`, `Object` and `Resumable`;
  - `Multipart`, `UploadHeader` and `Body`;
  - `Csek`, `StoredHeader` and `Time`.

  Bucket resource parsing uses `Input`, `Project`, `Catalog` and `Time`.
  Multipart framing, upload headers, bounded bodies and stored media headers
  use `Multipart`, `UploadHeader`, `Body` and `StoredHeader`. The upload driver
  distinguishes a resumable body abort from other storage failures on the
  typed session error. The multipart parser returns `MultipartError`; the adapters
  convert it through `ServiceError`. The other variants enter with the paths that
  produce them. A size test decides which variants are boxed.
- **JSON.** `ApiError` moves to `src/json/error.rs` with private fields. Its
  status, reason, location, plain and HTML choices are made in the JSON
  mapping from the domain variant, reproducing today's constructor calls.
  `ErrorKind` variants that exist only for reverse mapping are deleted.
- **gRPC and XML.** `From<ApiError>` for `Status` and for the XML error are
  deleted. A new failure class then fails to compile in every adapter instead
  of falling through to `Status::internal`. XML keeps HTTP statuses for its
  own codes.
- **`.message` extraction** goes: `url.rs`, `config.rs`, `storage::catalog::admin`,
  `import/filesystem.rs`, `seed/error.rs`, `auth`'s registry and
  `notification::event` use the typed validators and keep their texts.
  The configuration draft validates `seed_project` with `validate_project_id` and
  `InvalidProjectId::MESSAGE`. Import reports render the fixed metadata
  validator's error directly and use the model's bucket name validator.
- **Cause vocabulary.** `BackendCause`, `IoCause`, `BackendFailure`,
  `MemoryExhausted`, `Source`, `Link` and a `Layer` trait move to the
  foundation module `cause`.
  - `Source` holds `Box<dyn Layer>` instead of a closed list of upstream
    types, which removes the failure–bucket–object cycle.
  - `storage::failure` keeps the one `fn unavailable`, `Classified` and the
    context helpers.
  - `Link::Resume` goes: `resume_import` returns its `ResumeError` directly.
  - Remove the temporary `503 Service Unavailable: ` prefix from
    `StoredTimestampError` in this slice. It preserves the existing log text
    until the cause vocabulary moves.

### Display and `source()`

- **Convention.** A cause's text appears once in a report. Errors with at
  most one cause follow one of these conventions:
  - a **leaf** with no source;
  - a **wrapper** whose Display describes only its own layer and whose
    `source()` returns the cause;
  - **transparent**, whose Display is the cause's (optionally prefixed) and
    whose `source()` returns the cause's source.
- `ImportError::Cleanup` carries two failures. Its journal text gives the
  original failure first and the cleanup failure second. Its `source()`
  follows the original failure's underlying cause, so a report can place that
  detail after the cleanup failure. The Cause vocabulary slice will address
  report ordering while preserving the journal text.
- Transparent CLI, import, resume and adapter errors forward their cause's
  source. Texts the import journal stores stay byte-identical. Storage
  classification still reads typed source links for backend guidance, SQLite
  result codes, memory budgets and journal refusals. The Cause vocabulary
  slice removes their remaining repeated text while preserving those decisions.
- **Enforcement:**
  - a test helper that fails when a link's text equals or ends with the next
    link's, called from every exhaustive error table;
  - `Report` panics on a repeat in tests. Separate test-only tables identify
    known rusqlite source pairs (`src/error.rs`) and the classified links
    deferred to Cause vocabulary (`src/storage/failure/repeated_source_tests.rs`).
    Tests pin the repeats so a stale exception fails;
  - the `text::error_displays_source` rule rejects a `#[error]` format that
    interpolates its own source.
- **`Report`** formats notification lookup causes too. It becomes a plain
  join after the storage classification changes.
- **thiserror.** Types are converted module by module. `#[from]` never replaces
  a `classified_from!` conversion, and the ten `TypedSource` types keep their
  hand-written `source()`. `SeedError` also implements `source()` by hand so
  `Entry` and `Import` expose the error inside their box and `Unopenable`
  can expose its static cause.

### `io::Error::other`

About 83 production sites remain once tests and doc comments are excluded:

| Kind of site | Replacement |
| --- | --- |
| Broken invariants with literal text (RAM backend drafts, durability, reader) | `BlobError::{Missing, Poisoned, Closed}` and small typed errors |
| Wrapped allocation and integer conversions | `BlobError::{Allocation, Size}` with the std error as source |
| Concrete crate errors wrapped in `io::Error` (shutdown, startup, import, TLS, publisher, healthcheck probe) | typed `PublishError`, `ProbeError`, `TlsError`, `ImportError` variants and a typed shutdown result |
| HTTP client protocol failures with literal text | `PublishError` and `ProbeError` variants |
| Task and channel plumbing | typed up to the function that must return `io::Result` |
| Input limits inside stream bodies | a typed refusal up to the `AsyncRead` or `chunk_body` boundary |

Every new storage variant classifies as `Classified::Io` with kind `Other`, so
the wire text `storage I/O error (other error)` is unchanged.
`std::io::Error::other` then joins `disallowed-methods`, with a reasoned
expectation where a trait requires `io::Error`.

## Catalog execution

### Rules

- **Routing.** A catalog closure runs as a writer job if it can change rows,
  main or TEMP, or reads a writer-only TEMP table (`resumable_session_counts`,
  `notification_runtime`, `lifecycle_quarantine`). Every other closure runs
  on the read pool, unless a named reason in `SerializedRead` keeps it on the
  writer. The reasons are the advisory upload check and its XML equivalents,
  and the writer-only TEMP tables.
- **Snapshot.** A pooled read that issues more than one statement opens
  `sql::read_snapshot` first.
- **Thread I/O.** No closure on an executor thread, writer or reader, reaches
  a durability seam: `sync_dir`, `unlink`, `publish_blob`, a file sync or a
  staged-file finish.
  - Allowed: hard-link pins, opening an immutable or appendable data descriptor
    inside a read snapshot or transaction, RAM-backend publication, and reset
    and reseed under their allowance.
- **Ordering.** What a commit depends on for durability finishes on the
  blocking pool before the commit job is queued. Reclamation runs after the
  reply, and only when the transaction journaled something.

### Why pooled reads stay consistent

- **Read-your-writes.** A WAL reader sees every commit that finished before
  its first statement. The writer replies only after COMMIT returns, so a read
  that starts after its caller learned of a write sees it.
- **Concurrent writes.** Writes the caller has not been told about may or may
  not be visible, as through the serialized path today.
- **Authorization.** It does not weaken: the moved reads are advisory or
  decorate a response, and each mutation checks its prerequisites in its writer
  transaction. Rewrite registration and publication recheck the destination. An
  intermediate rewrite checkpoint checks the source, token and deadline.
- **Unlinked blobs.** A blob unlinked after a pooled snapshot fails its open,
  and the read retries on the serialized path, as reads do today.

### Enforcement

- `run_serialized_read(reason, op)` gives its closure `&Connection` only, so
  `transaction()` and `immediate()` do not compile there. Test builds also
  assert that its change count did not move.
- `fault::record_catalog_thread_io` records caller locations at every
  synchronous durability seam: `sync_dir`, `unlink`, `publish_blob`,
  `BlobBackend::create_draft` and the file syncs of staged writes, appendable
  data, derived recovery, admin publication and the retired manifest.
  In test builds, `Catalog::close()` panics when its data
  directory recorded executor-thread I/O outside an allowance. Every test that
  closes a catalog then checks the rule; its first full run found the partial
  pins a failed source batch unlinked inside the writer job.
- A reply whose caller was cancelled is dropped on the blocking pool, so a
  returned value that owns files never releases them on the executor thread.
- A test-build counter of `SQLITE_READONLY` answers on the read pool is
  asserted zero at close, which catches a moved job that secretly writes.
  Production still falls back to the writer silently.
- **End state.** `sql::exec`, the journal helpers and savepoints take a
  prepared `&WriteTx`, so a pooled read cannot call them at all.

### Jobs that move to the read pool

| Job | Note |
| --- | --- |
| `has_bucket_permission`, `get_policy` | `read_snapshot` instead of a transaction |
| `read_acl`, ACL projections, `bucket_acl_projections`, `object_acl_snapshot` | the snapshot is cloned inside the closure |
| `list_buckets_page_among` | one statement |
| `list_objects`, live and versions | under `read_snapshot`, so the page and the bucket metageneration agree; the soft-deleted listing purges and stays on the writer |
| gRPC `open_readable`, `get_readable` | a test pins the answer when finalization unlinks between snapshot and open |
| `session_by_id`, `session_by_token` | the guarded updates stay authoritative |
| `rewrite_buckets` | one snapshot of destination authority, conditions and both buckets' placement; registration and publication recheck destination authority |
| `next_notification` | one statement on the main table |
| soft-deleted gets, XML part and upload listings, HMAC key and notification reads, `xml_completion_parts` | second batch, after reading each in full |

The reader statement cache grows from 32 to 96 statements when listing moves.

### Round trips

W counts writer jobs and R counts reader jobs. The projection rows compare
nonempty responses immediately before and after bulk ACL projection, with
point reads already on the pool. The XML disk fixture measured 3 W per part and
9 or 11 W for one- or three-part completion before completion reused the
validated part rows. These counts include three disk cleanup jobs to select,
acknowledge and check the journal. The delete and upload rows count standalone
bucket queries. The trace test also checks for zero retention subqueries after
the change.

| Change | Before → after |
| --- | --- |
| XML part: check pending cleanup in the commit job and probe references only after an error | new part after acquiring its lease: 3 W → 2 W |
| XML completion: reuse the rows from `xml_completion_parts` under the upload lease | disk, after acquiring the lease, publishing a new object and reclaiming one page of parts: N+8 W → 8 W |
| Maintenance: a busy sweep marks its key deferred and a release wakes only for a deferred key; a pass starts with one snapshot probe of every kind of due work and runs only the sweeps with work | about 12 W and 3 tasks per idle wake → 1 R, and no self-wake |
| Reclamation: each retiring transaction reports whether it journaled; all sites use `reclaim_if` | 1 W, a task and a permit per commit → none when nothing was journaled |
| Object delete: authorize once, then purge and retire under one immediate transaction | strict delete via a public bucket ACL, on a versioned bucket: 1 W both ways; 2 transactions → 1; 9 bucket queries (3 settings/project, 6 for public access prevention) → 1; 2 policy reads → 1; 4 bucket ACL decodes → 1 |
| Upload publish: carry the authorized bucket's settings and project into ownership and ACL publication | permissive new object, after admission and advisory authorization: 1 W both ways; 4 bucket queries (authorization, settings, owner, ACL) → 1 |
| `buckets.list` with `projection=full` on N visible buckets, counting the page read | N W + (N+1) R → 2 R (permissive)<br>N W + (2N+1) R → 2 R (strict admin) |
| `buckets.get` with `projection=full`, counting the metadata read | 1 W + 2 R → 2 R (permissive)<br>1 W + 4 R → 3 R (strict admin) |
| JSON `objects.list` (live and versions) and gRPC `open_readable` and `get_readable` on the read pool, before ACL projection | JSON: 1 W → 1 R (permissive), 1 W + 1 R → 2 R (strict admin)<br>gRPC catalog open or get: 1 W → 1 R |
| `projection=full` on object get and list, counting only the additional projection | +1 W → +1 R (permissive)<br>+1 W + 1 R → +1 R (strict admin) |
| Rewrite, first call: source permission and pin, destination check and both buckets. Continuation: reuse the pooled token lookup for the lease, then reread under the lease | first pre-checks: 2 W + 3 R → 1 W + 2 R; completed replay after point reads moved to the pool: 1 W + 2 R → 1 W + 1 R |
| Admin batch cleanup: one reference query and one file job for the batch | 2N → 2 |
| Pub/Sub outbox poll on the pool | 1 W per write → 1 R |
| Writer settings: `keep_plans_across_bindings` (QPSG) as on readers; prepared `BEGIN IMMEDIATE` and `COMMIT`; a written reason for `wal_autocheckpoint = 256` | CPU only |
| Read cache: invalidation still swaps the map before the reply, and the stale files close in a tracked blocking task | up to 1,024 `close(2)` calls on the writer → none |
| BLOB columns read through `get_ref` | one allocation less per row |
| Lifecycle scan: statements batched per page | to count at slice start |

`read_pool.rs` stops holding the idle-pool mutex while it starts an executor
thread: the pop moves into its own statement. Derived inputs hold independent
disk hard links or RAM blob IDs, so the final deletion journal sweep reclaims
retired sources. A failure draining that journal belongs to the deletion
journal sweep in the maintenance diagnostic.

### Catalog structure and shutdown

- **`Inner` split.** Its fifty fields group into:
  - immutable `Policy`;
  - `Storage`, `Writer` and `Reads`;
  - `Admission` and `AdminState`;
  - `WriterRegistry`, with the deferred flag;
  - `Maintenance` and `Notifications`;
  - `Shutdown`.

  Behavior moves onto these structs, which shrinks the 54-file `impl Catalog`.
- **`CatalogOptions`.** `Catalog::open(backing, CatalogOptions)` receives:
  - clock and principals;
  - session limits and default soft-delete retention;
  - lifecycle execution;
  - parent public-access-prevention projects;
  - notification endpoint;
  - local admin.

  Inside open, before the executor starts, it installs the parent
  public-access-prevention table and reconciles authentication. That removes:
  - the `OnceLock`s and the `AtomicU64` retention;
  - `AuthenticationAlreadyConfigured`;
  - the silent `unwrap_or_default()` of session limits;
  - sixteen `.get().cloned()` sites.

  Startup keeps its order and diagnostics. No typestate: hundreds of tests use
  `Catalog` directly, and a typestate would only guard two start calls.
- **Shutdown.** `close()` cancels one token, closes admission, waits for the
  tracker, closes the read pool, drains the writer and runs the last job.
  Every waiter selects on `token.cancelled()`, so the seven hand-sent wakeups
  and six semaphore drains go. A parent passes its tracker token to a child
  before spawning, so the count cannot reach zero early. The server's
  `watch::Receiver<bool>` flags can become child tokens afterwards.

## Body I/O and protocol adapters

### Coalesced staged writes

`StagedFile` in `objects/durability.rs` keeps its name and gets new internals.

- **Buffering.** It holds a std `File` and two batch buffers that grow
  lazily up to `WRITE_BATCH_BYTES` each.
- **`write`.** It copies each incoming frame into the current batch.
  A full batch goes to one supervised blocking job, which calls
  `write_batch` and returns the buffer for reuse. Inside the job, in order:
  1. hit `BodyWrite`, then `write_all`;
  2. on bounded platforms, once the write-behind window fills, hit
     `BodyWriteBehind`, run `sync_data` and record the sync.

  The receiver fills the second buffer while the job writes the first.
  Truncation, final sync and release of the upload lease wait for the write.
  Dropping a stage revokes a queued job. A job already executing owns the
  file and the upload or writer lease until it returns on the blocking pool.
  A new writer therefore cannot truncate the stage during the old write.
  After a worker failure, only rollback may truncate and sync the old prefix.
- **`make_durable`.** One job drains the batch and syncs, in order:
  `BodyFlush`, the final write, `BodySync`, then `sync_all` or
  `sync_ordered`. A std write returns its error before the sync starts, so a
  failed tail can never be published as durable. AGENTS.md's flush-before-sync
  rule is reworded to "await the last write before syncing".
- **Rollback and reopening.** Rollback waits for the submitted write, truncates
  and seeks in one job, then syncs the acknowledged prefix. `open_output`
  opens, checks, truncates and seeks in one job.
- **Batch size.** `WRITE_BATCH_BYTES` starts at 64 KiB, today's bound in
  DESIGN-070, and moves to 256 KiB only under the measured-limit rule. The
  measured run is one 1 GiB HTTP/1 upload and 1,000 uploads of 100 KiB at
  concurrency 8, comparing server CPU seconds and wall time.

  Both alternating local macOS arm64 runs used 64 KiB batches. This
  comparison did not measure a 256 KiB pipeline. Each cell
  gives elapsed seconds / server CPU seconds for the two runs.

  | HTTP/1 workload | Tokio file | One write in flight |
  | --- | --- | --- |
  | 1 GiB | 68.87 / 5.29, 7.30 / 4.41 | 9.90 / 3.61, 7.32 / 3.63 |
  | 1,000 × 100 KiB, concurrency 8 | 4.73 / 1.51, 1.72 / 1.40 | 4.87 / 1.51, 1.29 / 1.09 |

  Awaiting each full batch before receiving another took 22.67 / 3.99 and
  1.60 / 1.20 seconds in a separate run. That wait serializes hashing and
  disk writes. Other builds were active in both Tokio runs and the first
  pipelined run. The first Tokio 1 GiB upload took 68.87 seconds with
  four rustc processes and a linker active. The second pipelined run
  observed no build processes.
  With one write in flight, neither paired workload exceeded its previous
  elapsed time by 10%, and average server CPU seconds fell on both.
- **Hashing** stays per frame on the receiving task, before the copy. Moving
  it into the batch job would hold the eight shared blocking threads longer
  under concurrent uploads. Size checks and checksum verification keep their
  order: verify, then make durable.
- **Fault tests.** Tests that inject a fault mid-body grow their bodies to at
  least three batches, so the fault still lands mid-body.

### Single blocking jobs for copies

- **Re-hash.** Resumable finalization re-hashes in one job that reads
  positionally from offset 0 into a pooled 1 MiB heap buffer.
  - The pool is the existing `LARGE_BUFFERS`.
  - Its memory is bounded by the blocking threads, not by requests.
  - The re-hash itself is kept, as
    [resumable upload](resumable-upload.md) requires.
- **Derived advance.** One job restores the draft, opens the output, copies
  the source and makes the output durable. Admission is checked every 1 MiB,
  and the memory backend's `yield_now` goes.
- **XML assembly.** It reads its part rows once, then runs one job per part
  (copy and verify) and a final durable job. The whole-object CRC32C comes from
  combining part CRCs (`compose_crc32c::combine`).
- **Large stack frames.** These remove the 64 KiB stack arrays from every
  async function. `clippy::large_futures` then joins the gate, with a reasoned
  expectation or `Box::pin` at the request dispatch for futures of 17–24 KB.

### Single-source copies

Derived transfers already pin each source with a hard link before copying it
(`derived/source.rs`). Soft-delete restore already publishes such a pin as a
live blob (`publish_pinned`).

- **When it applies.** One source, and destination bytes equal to the source
  bytes: `objects.copy`, XML copy, and a rewrite that keeps location, storage
  class and stored bytes.
- **What it does.** The transfer publishes the pin as the destination blob and
  records the source's stored hashes.
  - These paths already finish in one call, so call counts, `done`,
    `totalBytesRewritten` and rewrite tokens do not change.
  - No bytes are copied, so the copied-bytes verification has nothing to
    check. The source hashes were verified when the source was written.
  - The memory backend shares the stored bytes.
- **Checks at slice start:**
  - whether a customer-key change alters stored bytes (if so, that rewrite
    keeps the copy);
  - that nothing else on these paths, such as gzip transcoding, alters bytes.
- **Tests.** Publication gets the crash and `Fault::PowerLoss` tests at the
  rename and directory-sync seams.

### Pull-based gRPC `ReadObject`

- **The stream.** `ReadObjectStream` implements `Stream` over `ObjectReader`
  with three states: idle, reading (a boxed future returning the reader and
  the next chunk) and finished.
  - It tries a read already at hand first, as the HTTP body does.
  - The metadata rides on the first data message, and an empty object still
    gets one message.
  - `ObjectReader` takes a two-variant `FrameLimit { Http, Grpc }`, so both
    protocols share one implementation.
- **Cancellation.** Dropping the stream releases its reader and cancels a queued
  read. A positional read already executing in the OS holds its file until that
  call returns. There is no producer to read another chunk after cancellation.
- **Panics.** A panic resets the HTTP/2 stream through the supervised executor.
- **Message size.** `ReadObject` uses up to 256 KiB per message. In a single
  1 GiB h2c read, 256 KiB took 1.57 server CPU seconds against 2.27 at 64 KiB.
  Server RSS with 32 concurrent streams rose from 21,728 KiB to 34,928 KiB.
- **Boxing.** The response body is boxed once instead of twice.
- **Go SDK comparison.** On macOS arm64 with Rust 1.100.0 release builds,
  Go 1.27.4 and the Storage SDK v1.71.0 read the same 1 GiB object to
  `io.Discard` over loopback h2c. Each run started a fresh server. In each
  round the order was push, pull, then pull with one frame read ahead.
  All three used 256 KiB messages. Each cell gives elapsed / server CPU seconds.

  | Implementation | First round | Second round |
  | --- | --- | --- |
  | Producer and one channel slot | 0.603 / 0.97 | 0.585 / 0.99 |
  | Pull on demand | 0.571 / 1.01 | 0.589 / 1.01 |
  | Pull with one frame read ahead | 0.640 / 1.00 | 0.599 / 0.95 |

  The acceptance criterion was elapsed time at most 10% above push. On-demand
  pull met it in both rounds, avoided a queued frame per stream and started
  disk reads only when polled. It used 0.02–0.04 more server CPU seconds than
  push. Two runs alongside one or two rustc processes and four cargo processes
  cannot distinguish that difference from run-to-run variation.
- **Scope.** `BidiReadObject` stays push-based.

### Multipart and batch parsing

`memchr` (2.8.3, already locked) becomes a direct dependency under the
dependency policy.

- **`multipart::Delimiter`.** It holds the delimiter and a
  `memmem::Finder`, used by both multipart and batch parsing.
- **Body mode** emits zero-copy slices of the received frame.
  - It keeps a carry of fewer bytes than the delimiter.
  - It searches the seam between the carry and the next frame in a scratch
    buffer of at most 146 bytes. Each side holds at most 73 bytes, one fewer
    than the longest delimiter (CRLF, two hyphens and a 70-byte boundary).
- **Header mode** copies only what lines need, as today.
- **Tests.** The existing tests that parse at every split width cover seams
  and carries. A property test compares random splits of bodies full of
  delimiter prefixes against the whole-body parse. A test asserting 8 KiB
  chunks pinned the old copy size, not a behavior; it now asserts the frame
  size and the carry bound.

### Smaller items

| Item | Design |
| --- | --- |
| gRPC resumable tail | a `BytesMut` reserved once per stream, split when whole granules persist; each byte is copied once |
| Deadline renewal per frame | a per-connection progress clock; the watch notifies only on state changes, and an expiring deadline re-arms from the last frame instead of expiring. Same expiry instant. Lands last among performance slices, and only if its measured run shows a gain |
| Query parsed three to five times per request | parsed once into `RequestCtx` on first use, so the first error appears where it does today; `percent` clones a key only for a duplicate error |
| ACL subjects built for every listed object | built only for `projection=full` |
| `diagnostics::emit` | checks the level before locking stderr; per-connection debug events are built only when enabled |
| XML writer | escapes through a `fmt::Write` adapter and copies runs between special bytes found with `memchr3` |
| Strict JWT verification | a bounded cache of RSA results keyed by message and signature digests; expiry, issuer and audience are checked every request. Adopted only if verifier keys cannot change at runtime, or keyed by key-set generation if they can |
| gRPC upload hashing | the intake stops hashing and passes the expected hashes; the catalog's mismatch maps to today's gRPC wording in the write adapters |
| HTTP/1 head limit | the comment on `CONNECTION_READ_BUFFER` explains when an incomplete head gets 431; a test sends a 40 KiB head. `MAX_QUERY_BYTES` stays, since it still bounds gRPC v1 |
| `CONNECTION_READ_BUFFER` | stays 16 KiB unless the measured run (one 1 GiB upload at 16 and 64 KiB) shows a gain after coalescing. Raising it also raises the documented header limit, which the same change updates |

## Types, configuration and duplicated rules

### Record types

Persisted bytes do not change: names stay TEXT, generations INTEGER, custom
time its original text, completions the same TEXT. JSON in TEXT columns keeps
its bytes through `#[serde(transparent)]` and `#[serde(into = "i64",
try_from = "i64")]`.

- **Buckets.** `BucketKey(String)` is the stored bucket name, never
  re-validated when read. `AdmittedBucket` is a request's name, admitted under
  the server's compatibility profile; `BucketName` converts into it.
- **Objects.** `ObjectKey<'a> { bucket: &BucketKey, name: &str }` replaces
  the 83 `(bucket, name)` parameter pairs. `ObjectTarget` is its owned form.
- **Generations.** `Generation` and a new `Metageneration` come from one macro
  with SQL and serde conversions; a value of 0 or less fails decoding.
  Precondition values stay `i64`, because 0 and negatives mean something there.
- **Custom time.** `CustomTime { text, instant }` is parsed once, ordered by
  instant and rendered as its text. It replaces four re-parses.
  - Text that cannot be parsed keeps its spelling with an empty
    `Option<UnixNanos>`. This preserves corrupt stored rows for readers and
    defers request errors to the existing checks through `instant()`.
    Comparison sites order the returned `UnixNanos`, and equality keeps the
    original spelling significant.
  - A property test shows that values the current import can publish also
    parse as `CustomTime`. Preflight also accepts year zero and some longer
    zero-padded fractions that publication rejects.
  - Replay fingerprints keep hashing the text.
- **Resumable completion.** `SessionState::Complete(StoredCompletion)` with
  `Json(Map)` and `Proto(Vec<u8>)` replaces the separate `completion` field.
  - The stored text tells the variants apart: an object is JSON and an array
    of integers is gRPC.
  - A session URI used across adapters fails typed instead of serving the
    wrong shape.

### Configuration

- **Draft.** Today's `Config` becomes `ConfigDraft`. Every `check_*` keeps
  working on it, so diagnostics stay byte-identical.
- **Assembly.** `TryFrom<ConfigurationReport> for Config` builds nested types:
  - `Listeners`, `GrpcListener` and `TrustedListener`;
  - `Tls` with a `TlsIdentity`;
  - `Storage`, `Seed`, `Endpoints` and `Admin`;
  - `notifications::Runtime`;
  - `Limits`, `Timeouts` and `AuthSettings`.

  The checks that settle a pairing return their typed piece, so assembly needs
  no `unwrap`.
- **What goes.**
  - The `effective_*` methods become resolved fields. The one that depends on
    the bound address stays a method.
  - TLS pairing is checked once.
  - Fields become private behind accessors.
- **Tests.** Unit tests build drafts. Integration tests load configurations
  through the real parser.
- **Files.** `config.rs` splits into `config/draft.rs` and
  `config/assemble.rs`.

### One implementation per rule

| Rule | Shared home |
| --- | --- |
| Single header value (absent, one, repeated) | `http_header::sole` |
| Idempotency token | `object::IdempotencyToken`; each adapter keeps its messages, and XML copy still refuses any token |
| Anonymous canned ACL refusal and the uniform-access precheck | `acl::refuse_anonymous_canned_acl`, `BucketRecord::uniform_access` |
| CSEK hash visibility | `csek::StoredHashes::for_key`, used by the JSON resource |
| Default content type | `object::content_type::{DEFAULT, or_default}` |
| Calendar math | `time_format::civil`, including weekday calculation and checked against an independent day-by-day test. The standalone `xtask` keeps its ledger date implementation so it can validate ledgers without depending on the server. |
| XML precondition headers | `xml::conditions(headers, messages)`, keeping both message sets |
| Bucket conditions from the query | `json::query::bucket_conditions` and one table of parameter names |
| Host lowercase and trailing-dot removal | `net::normalized_host` |
| List page cap of 1,000 | `page_token::MAX_PAGE`, only where it is the list cap |
| `Expires` header | `time_format::http_date_value` |
| `x-goog-meta-*` headers | `object::upload_headers::custom_metadata_headers` |
| `text/plain; charset=utf-8` | `mime::TEXT_PLAIN_UTF8` |
| Percent unescaping in import | `percent::decode_bytes` |
| XML resumable ETag | `xml::Validators`, shared with `object_etag` |
| Same name, different constants or functions | `UNIFORM_ACCESS_UPDATE_BUCKET_ACL`, `UNIFORM_ACCESS_UPDATE_OBJECT_ACL`, `json::upload_content_type`, `object::upload_headers::content_type` |

### Robustness helpers

- **Guards.** The 31 drop guards get `#[must_use]` with a message.
- **Poisoned locks.** `crate::sync::{lock, read, write}` recover usable state
  and report once per poison episode. Checked acquisitions preserve the original
  failure or health-check signal and let callers repair state when recovery
  needs a rollback or directory sync. Cleanup can release resources while
  retaining poison for health checks. `Mutex::lock` and `RwLock::read`/`write`
  join `disallowed-methods` for production code.
- **String enums.** `string_enum!` moves to `choice` with optional serde and
  SQL arms. It absorbs `serialize_as_str!`, `sql::text_enum!` and
  `storage_classes!`, and the plain hand-written `FromStr` impls.
- **Diagnostic events.** Names live in `diagnostics::Event`, with the seven
  contract names marked and pinned by a test.
- **CLI dispatch.** The arguments are collected once and matched on literal
  slices. The parsing order and output are the same, and clap is still not
  used.

## Lints, tests and newer Rust

### Lints

| Lint | Where |
| --- | --- |
| `clippy::significant_drop_in_scrutinee` | gate; the read-pool lock already drops before branching |
| `clippy::or_fun_call` | gate; 9 sites |
| `clippy::ignore_without_reason` | workspace deny; no site |
| `clippy::return_and_then` | gate; 8 sites at slice start |
| `clippy::large_futures` | gate, after the body I/O slices |
| `disallowed-methods`: `serde_json::Map::remove`, `std::io::Error::other`, plain lock methods | each with its slice |
| `cargo::unused_dependencies` | workspace deny; available in Cargo 1.100.0 |

All of these exist in the pinned Clippy and Cargo 1.100.0 (checked
2026-11-26). Each rule gets its row in the Rules table.

### Property tests

These start DESIGN-098 for parsers:

- `percent`, `base64`, `page_token` and `decimal`: round trips, never
  panicking on arbitrary input, and the grammar boundaries;
- `time_format` and `civil`: round trips and order;
- multipart: any split equals the whole-body parse;
- `CustomTime` against the import grammar.

### Test helpers

- **Clocks.** One `ManualClock`, with a read counter, replaces the in-crate
  test clocks.
- **Shared helpers.** `tests/common` gets one clock and one base64 helper
  instead of nine copies.
- **Fixtures.** Record builders under `cfg(test)` replace full `ObjectRecord`
  literals, and replace `json::parse_insert` as a bucket fixture in 27 files.

### Newer Rust

Checked on rustc 1.100.0 on 2026-11-26.

- **`std::assert_matches!`** (with `use std::assert_matches;`) replaces 475
  `assert!(matches!(..))` sites in one mechanical commit, so failures print
  the value.
- **`str` and `[u8]` `trim_prefix` and `trim_suffix`** replace
  `strip_*().unwrap_or(..)`.
- **`cfg_select!`** is used inside function bodies in `fsutil` and
  `storage::catalog::fault`.
- **if-let guards, `fmt::from_fn`, `Duration::from_mins`, `map_or_default`
  and `extract_if`** are used where a slice already edits the code.
- **`core::mem::DropGuard`** is used only for closure-only cleanup in new code.
  The existing guards carry state and methods.

## Implementation order

- **Branches.** Each phase is one pull request into `main`, and each slice in
  it is one commit or a short series. A slice that moves code never also
  changes its logic.
- **Phase order.** The phases run in the order below.
  - Golden tests come first, so later phases have something to compare against.
  - After Safety nets and Quick wins, the catalog round-trip, body I/O, error
    model, and types, configuration and rules slices run in parallel. Catalog
    structure follows.
  - Run the large module moves in sequence after the parallel slices and
    catalog structure settle. This avoids conflicts with concurrent edits and
    moves code whose types and catalog interfaces are already in place. These
    moves still land before 1.0.0.
- **Checks.** Every slice runs:
  - `cargo xtask lint`, lowering baselines in the same change;
  - `cargo test -p xtask`;
  - the affected tests and `just check`.
- **Doc search.** Before finishing, each slice searches `README.md`,
  `docs/en`, `docs/ja`, `compatibility` and the SDK READMEs for the names it
  moved.

### Safety nets

| Slice | Scope | Acceptance |
| --- | --- | --- |
| Error goldens | shared case builders; JSON status, headers and body bytes for every domain error and modifier; XML bytes; gRPC `Status` for every constructor; a cross-adapter input matrix; old mappings kept as a test oracle until the error slices finish | the goldens pass on today's code |
| Persisted-format goldens | stored completion text, upload replay documents, replay fingerprints, the bucket record built through JSON in `admin.rs` | pass on today's code |
| Layer and adapter rules | `xtask` rules, baseline, Rules table, the layer table in design, AGENTS.md | `cargo xtask lint --baseline` equals the committed file |

### Quick wins

| Slice | Scope | Acceptance and docs |
| --- | --- | --- |
| Writer-thread I/O | XML part and completion syncs on the blocking pool; a failed source batch's pins and unclaimed replies released there; the read-pool pop; the close-time thread-I/O assertion with the reset and reseed allowance | new failed-sync, crash and power-loss tests for both XML paths; the full suite passes the assertion; design "Persistence and concurrency" states the rule |
| Writer settings | QPSG on the writer; the `wal_autocheckpoint` reason; the writer settings and schema version in design "Persistence and concurrency". The reader cache grows with the listing move, as [Jobs that move to the read pool](#jobs-that-move-to-the-read-pool) says | existing suite |
| Lint quick wins | `significant_drop_in_scrutinee`, `or_fun_call`, `ignore_without_reason`, `return_and_then`, `unused_dependencies`, `#[must_use]` on guards | Rules table |
| Small fixes | the head-limit comment and 40 KiB head test, the diagnostics level check, and a reference to `CATALOG_SCHEMA_VERSION` in `public-contract-review.md` | — |

### Catalog round trips

| Slice | Scope |
| --- | --- |
| Point reads on the pool | `run_serialized_read`, the read-only detector, the first batch of moved jobs |
| Bulk ACL projections | JSON buckets.list and buckets.get, object projections on the pool |
| Listings on the pool | `list_objects` and gRPC readable opens |
| Conditional reclamation | `reclaim_if` at every site |
| Maintenance on demand | the deferred flag and the probe |
| XML part rows once | completion reuses its rows; the part probe only after an error |
| Delete and upload authority | `BucketAuthority`; purge inside the delete transaction |
| Rewrite pre-checks | permission, buckets and continuation lookup on the pool |
| Admin batch cleanup | one reference query and one file job per batch |
| Stale read-cache files | closed off the writer |
| Prepared write transactions | `WriteTx` with prepared `BEGIN IMMEDIATE`, `COMMIT` and `ROLLBACK`, then `sql::exec(&WriteTx)` |
| Remaining reads and lifecycle | the second batch of moved reads; lifecycle statements per page; BLOB `get_ref` |

Each slice adds tests for each moved read:

- the pooled-read counter rises;
- a write followed by a read sees the write;
- an exclusive reservation sends the read to the writer;
- a failing reader falls back;
- the read-only counter stays at zero.

Each slice also adds a test-build writer-job counter that pins its before and
after job counts.

### Body I/O slices

| Slice | Scope | Measured run |
| --- | --- | --- |
| Coalesced staged writes | `StagedFile`, `open_output`, all upload paths | 1 GiB upload and 1,000 × 100 KiB at concurrency 8, with 64 KiB pipelined batches |
| Copies in single jobs | re-hash, derived advance, XML assembly, CRC combination, the `large_futures` gate | one 1 GiB compose and one 1 GiB resumable finalization |
| Single-source copies | publish the pin | one 1 GiB copy |
| Multipart with `memmem` | `Delimiter`, batch parsing, the `memchr` edge | one 256 MiB multipart upload |
| Pull-based `ReadObject` | the stream, `FrameLimit`, single boxing | 1 GiB at 64 and 256 KiB, RSS at 32 streams |
| Small items | resumable tail, query once, lazy ACL subjects, XML writer, JWT cache, gRPC intake hashing | metadata GET and listing 1,000 objects at concurrency 8 |
| Lazy deadline renewal | progress clock and re-arming | 1 GiB upload and download, CPU and context switches; dropped if no gain |

### Error model slices

| Slice | Scope |
| --- | --- |
| Display and source convention | the test helper, strict `Report` in tests, transparent CLI, import, resume and adapter links, notification logging through `Report`; classified storage links tracked by test-only exceptions |
| thiserror | the dependency records; used for new and changed types |
| Typed validators | the `.message` sites; `ApiError` out of the source list |
| `ServiceError` and `InputError` | bucket resource parsing, admin defaults and import through the domain call; the adapter mappings |
| Typed upload and multipart errors | upload headers, stored headers, multipart, upload failures |
| `ApiError` inside JSON | the shared helpers return service errors; both `From<ApiError>` impls deleted; private fields; `ErrorKind` pruned; oracle removed |
| `io::Error::other` | three commits (RAM backend; startup, shutdown, import and fsutil; publisher, probe and TLS), then the `disallowed-methods` entry |
| Cause vocabulary | the `cause` module, `Layer`, the classified storage links and removal of their temporary test exceptions, plain `Report` |
| thiserror conversion | module by module, then `text::error_displays_source` |

### Types, configuration and rules

| Slice | Scope |
| --- | --- |
| Neutral helpers | `http_header::sole`, `IdempotencyToken`, `normalized_host`, `http_date_value`, `TEXT_PLAIN_UTF8`, renamed constants, `decode_bytes` |
| Calendar and property tests | `time_format::civil` and the property tests |
| Domain consolidation | content type, CSEK hashes, canned ACL, bucket conditions, `x-goog-meta-*`, XML preconditions, the XML resumable ETag |
| Generation types | `Generation`, `Metageneration` |
| `CustomTime` | — |
| Bucket and object keys | catalog signatures first, then each adapter |
| `StoredCompletion` | — |
| Configuration | `ConfigDraft`, assembly, nested types, private fields |
| Enums and events | `choice` arms, merged macros, `diagnostics::Event` |
| Test hygiene | clocks, base64, record fixtures, `assert_matches!` |
| Newer Rust and CLI | `trim_*`, literal CLI matching, lock helpers |

### Catalog structure

| Slice | Scope |
| --- | --- |
| `CatalogOptions` and `Inner` | options at open, sub-structs |
| Shutdown | token, tracker, `close`, the `tokio-util` edge |
| In-process derived state | compose and multi-step copies keep offset and hasher in memory |

### Layers and structure

| Slice | Scope |
| --- | --- |
| Foundation moves | the RFC 3339 parser, private `fsutil` and `net` |
| Model value types | `profile`, URL helpers, auth choices, validator callers |
| JSON-owned rules out of `json` | `Projection`, component counting, fixed metadata, bucket insert parsing, the two JSON round trips, authority helpers |
| Notification renderer | injected at catalog open |
| `auth` split and `HmacKeys` | — |
| `fsutil` per OS | `cfg_select!` inside bodies; `scripts/check-cross.sh` and the portability CI job |
| `service`, part one | the shared request pieces and `State`; ledger IDs under `server::copy` become `service::copy` |
| gRPC listener | into `server` |
| JSON handlers and `server.rs` files | `json::handler`, `xml::read`, the transport split |
| Config documents | the parse half moves to the settings layer |
| Protobuf crate | generated code in its own crate, with the build measurement |
| Public facade | `app`, `testing`, private modules, `pub(crate)`, dead code. The `Config` listener accessors are public for the startup binary. |

### Last

| Slice | Scope |
| --- | --- |
| Behavior decisions | the owner's choices, with ledger rows, gaps, release note and docs |
| Test layout | after code moves: `*_tests.rs` files and directories moved, ledger IDs renamed, the `xtask` check |
| Final verification | full suite, `just check-sdk`, the nine-SDK scorecard |

The release candidate then follows the [release policy](../release-policy.md#the-100-gate).

## Acceptance cases

The ledger records these as DESIGN-105 cases:

| Case | Accepted when |
| --- | --- |
| `DESIGN-105-GOLDENS` | Error, persisted-format and bucket-default goldens recorded before the code moved pass after every slice. |
| `DESIGN-105-WRITER-IO` | No executor thread reaches a durability seam outside the reset and reseed allowance; every test that closes a catalog checks it. |
| `DESIGN-105-READ-ROUTING` | Read-only jobs run on the read pool, see completed writes, fall back under exclusive reservations and failures, and never answer `SQLITE_READONLY`. |
| `DESIGN-105-ROUND-TRIPS` | Writer and reader job counts per operation match the round-trip table. |
| `DESIGN-105-BODY-IO` | Staged writes and copies run as bounded blocking jobs, with failed-write, failed-sync, crash and power-loss coverage for each path. |
| `DESIGN-105-PROTOCOL` | Multipart and batch parsing, gRPC `ReadObject` and the smaller adapter items keep their wire behavior. |
| `DESIGN-105-LAYERS` | The layer and adapter rules are zero tolerance. |
| `DESIGN-105-ERRORS` | Adapters map protocol-neutral errors exhaustively; `ApiError` is private to JSON; chains repeat no text; `io::Error::other` remains only under reasoned expectations. |
| `DESIGN-105-TYPES` | Records, sessions and configuration carry the typed fields above with unchanged persisted bytes and diagnostics. |
| `DESIGN-105-RULES` | Each rule in the consolidation table has one implementation. |
| `DESIGN-105-LINTS` | The new lints are in the gate and the property tests pass. |
| `DESIGN-105-LIFECYCLE` | The catalog opens with fixed options and shuts down through one token and tracker. |
| `DESIGN-105-SURFACE` | Only `app` and `testing` are public. |
| `DESIGN-105-BEHAVIOR` | The owner's behavior decisions ship with tests, ledger updates and a release note. |

## Limits

- **Not a performance program.** Measured runs are single facts at local
  scale, not baselines or gates.
- **Supply-chain policy is out of scope.** Cargo 1.100.0 has a
  `min-publish-age` resolver gate (`registry.global-min-publish-age`). Adopting
  it is a supply-chain policy change for the owner, not part of this program.
- **Left in place.** `BidiReadObject`, the write path's per-frame hashing and
  the upload advisory check keep their current design.
