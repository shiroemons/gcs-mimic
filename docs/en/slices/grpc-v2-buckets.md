# Storage v2 bucket operations

<!-- markdownlint-configure-file {"MD013": false} -->

Scope: RPC-STORAGE-001–004, the bucket portion of BEHAVIOR-RPC-001 and AUDIT-008. All broader rows remain PARTIAL until their remaining contracts are verified.

## Acceptance contract

- Parse `projects/_/buckets/NAME`, reject malformed names with INVALID_ARGUMENT, and decode `x-goog-request-params` routing metadata without allowing it to override the request.
- Get existing buckets, return NOT_FOUND for missing buckets, enforce both metageneration conditions, and project supported read masks. Unsupported mask fields return UNIMPLEMENTED with ErrorInfo.
- Create using empty or `projects/_` parent and `bucket.project = projects/<id>` (including `_`), with the JSON-supported metadata subset. Duplicate names return ALREADY_EXISTS. Unsupported semantics return UNIMPLEMENTED rather than being ignored.
- List by project and prefix with bounded page size and project/prefix-bound continuation tokens.
- Delete empty buckets with atomic metageneration conditions; nonempty buckets return FAILED_PRECONDITION and retain their contents.
- Reuse shared authorization: strict requests denied without credentials return UNAUTHENTICATED and registered callers without permission receive PERMISSION_DENIED. Anonymous Get/Delete honors effective allUsers grants and public access prevention, exactly like JSON.
- Ordinary Go `storage.NewGRPCClient(ctx)` supports Bucket.Create, Bucket.Attrs, Buckets iteration and Bucket.Delete with omitted optional arguments.
- JSON-created buckets are visible through gRPC and gRPC-created buckets are visible through JSON.

## Boundaries and sources

No setting, dependency, storage implementation or startup change is needed. CreateBucket reuses shared typed retention, soft-delete, PAP, CORS and Autoclass validators. Creation still builds a JSON insert resource for the remaining creation fields; a shared creation builder is not yet implemented, and this operation stays PARTIAL. UpdateBucket constructs shared typed updates directly. Both use shared bucket authorization and the transactional catalog. Bucket deletion and listing retain catalog concurrency and memory bounds.

Deletion implements the shared native empty-bucket contract. Provider bucket soft-delete/restore, unfinished XML multipart uploads and Rapid state interactions are outside this slice; object soft deletion is not a claim of bucket soft-delete support.

Create supports location, storage class, labels, CORS, versioning, default event hold, integral retention/soft-delete durations and IAM uniform access/public access prevention through the same native JSON validation. Empty or disabled unsupported configurations that equal their protobuf defaults, and `rpo=DEFAULT`, are accepted. Autoclass uses the shared configuration rules in [bucket-autoclass-storage-class](bucket-autoclass-storage-class.md); output-only Autoclass times are rejected. Object retention is enabled by `enable_object_retention` ([SDK parity](grpc-v2-sdk-parity.md)). Nonempty lifecycle, ACL, predefined ACLs, billing, encryption, logging, custom placement, hierarchical namespace and IP filtering remain UNIMPLEMENTED. Output-only policy fields and fractional durations are rejected explicitly.

Read masks support the top-level fields `name`, `bucket_id`, `etag`, `project`, `metageneration`, `location`, `location_type`, `storage_class`, `autoclass`, `labels`, `create_time`, `update_time`, `default_event_based_hold`, `versioning`, `cors`, `lifecycle`, `retention_policy`, `soft_delete_policy`, `object_retention` and `iam_config` ([SDK parity](grpc-v2-sdk-parity.md)). Absent/empty masks and a single `*` return this converted subset. Nested paths, unsupported fields and mixed wildcards return UNIMPLEMENTED. List uses the same mask rules; `return_partial_success=true` remains unsupported. Page size defaults to 1000 and is capped at 1000; a negative size is invalid. Tokens are opaque project/prefix-bound catalog cursors shared with JSON, not provider-compatible token bytes.

Every Storage v2 RPC validates routing metadata at the common entry point.
Routing follows the pinned proto's bucket/project captures and source_bucket for
rewrite sources. Streaming calls compare the first message; rewrite continuations
compare the token's bound names. Values are form-percent-decoded and conflicting
values fail INVALID_ARGUMENT. Create compares project against bucket.project.
Rapid writes also compare appendable. Go direct_connectivity_diagnostic is
accepted as diagnostics. Other semantic keys, including redirect tokens, return
UNIMPLEMENTED. Routing cannot change catalog selection or authorization.
Bucket IDs follow the configured native or fake-gcs-server naming profile in
both bucket and object RPCs. Provider-exact invalid-name status and complete SDK
routing combinations remain pending in SPEC-GRPC-ROUTING-NAMES.

`x-goog-user-project` is ignored on every Storage v2 RPC, including an empty value, as the JSON API ignores userProject since 2026-11-13 (SPEC-SDK-UNSUPPORTED): a local bucket cannot enable Requester Pays, so the billing project cannot change a result. The default Go client does not send this header, and `Bucket.UserProject` with or without a project is accepted alike. Shared `status::from_json` converts adapter failures consistently, including backend HTTP 503 to UNAVAILABLE and unsupported emulator semantics to ErrorInfo-bearing UNIMPLEMENTED even when JSON represents those limitations as HTTP 400.

The [pinned Storage v2 proto](https://github.com/googleapis/googleapis/blob/933dd1be8180a22bca2dc473b53d4f4297ee3b0c/google/storage/v2/storage.proto) was retrieved and its SHA-256 matched `a0df6132967b97a89be248f1f24d277ee8df8f8d885db1d4af6249469ff0c1cf` on 2026-10-27. In particular, CreateBucket.parent is empty or projects/_; the owner belongs in bucket.project. The foundation's project ID rather than numeric project resolution limitation remains. Storage v1 and object cross-transport semantics are outside this slice.

## Verification

On 2026-11-04, all 90 Storage v2 library tests passed. The object billing/routing
wire regression first failed with NOT_FOUND instead of UNIMPLEMENTED on the old
implementation. Additional tests cover resumed upload bucket matching, detailed
malformed-route errors and creating/writing/reading a two-character legacy bucket.
No SDK lane or provider request was run for this routing change.

Acceptance was recorded before implementation. The new ordinary Go SDK journey first failed at Bucket.Attrs with UNIMPLEMENTED on the unchanged implementation. The fixed official proto was retrieved and its digest verified; official Rust stable metadata confirmed Rust 1.99.1. No dependency was added or updated.

On 2026-10-28:

- `cargo check --workspace --all-targets --locked --offline`: PASS before tests.
- `cargo test --locked --offline --lib grpc::v2::`: PASS, eight tests covering the five bucket tests, principal handoff, names and conversion. CRUD, metadata conditions, masks/ErrorInfo, metadata/default validation, routing, pagination, strict/public authorization, explicit billing metadata rejection and common status conversion are covered.
- `GCS_MIMIC_GO_TEST_RUN='^TestGRPCBuckets|^TestGRPCFoundation' bash scripts/sdk-go.sh`: PASS, Go Storage v1.70.0 ordinary Create/Attrs/iterator/Delete, one-item pages, cross-protocol visibility, duplicate and nonempty protection, conditions, strict/public authorization and empty UserProject omission. Remaining unsupported operations retain immediate ErrorInfo-bearing failure; only implemented bucket calls were removed or replaced in unsupported assertions.
- `just check-sdk`: PASS, complete Go and Ruby HTTP suites, including Ruby origin self-check, migration, permissive/strict metadata/object journeys and signatures. The existing Go V2 signer test explicitly skips in this HTTP profile because it requires HTTPS; no additional HTTPS run was performed.
- `RUST_TEST_THREADS=8 just check`: PASS on the final source, native/Windows/Linux clippy, all Rust unit/integration/doc tests (518 library tests and both gRPC process tests), ledger/generated views and offline shell/container-publication checks. No failed or ignored Rust tests.
- `npx --yes markdownlint-cli2 docs/en/slices/grpc-v2-buckets.md docs/ja/grpc-v2-buckets.md`: PASS, zero issues.
- `cargo xtask compatibility render`: PASS, both generated matrices use the six updated ledger rows.

No container build, guide walkthrough, benchmark or real-cloud run was performed for this RPC-only change. Provider-exact error details/precedence, numeric project resolution, omitted fields and object cross-protocol acceptance remain unverified; all affected ledger rows remain PARTIAL.

On 2026-11-14 `grpc_bucket_rpcs_reject_unsupported_options_without_side_effects` pinned the bucket rejections that had no test: CreateBucket `predefined_acl` and `predefined_default_object_acl` (no bucket is created), GetBucket nested and mixed-wildcard `read_mask` paths, and ListBuckets `return_partial_success`. Each returns UNIMPLEMENTED with ErrorInfo naming the option. `cargo test --locked --offline --lib grpc::` passed 168 tests. RPC-STORAGE-002, 003 and 004 record this as the passing case `grpc_unsupported_option_rejections`; their other pending cases are unchanged.
