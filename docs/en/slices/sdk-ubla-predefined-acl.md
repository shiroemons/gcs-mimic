# Uniform bucket-level access and predefined ACL rejection

Scope: DESIGN-019, BEHAVIOR-BUCKET-013 and the iamConfiguration input subset of
JSON-BUCKETS-005. Contract checked on 2026-10-27; the feature ledger owns status.
Both broad behavior rows remain PARTIAL because ACL effects and provider edges
remain pending.

## Acceptance contract

- Insert, get, list and restart preserve enabled uniform access and its deadline,
  90 consecutive days after activation, using the injected clock.
- The deprecated bucketPolicyOnly alias reads and writes the same state.
- Omitted, null, empty and disabled create settings mean disabled access.
- PATCH and PUT toggle access with storage.buckets.update and atomic
  metageneration conditions; repeated activation preserves the deadline.
- Disabling at or after the deadline fails without changing metadata.
- Terraform-style creation combines location, uniform access and enforced PAP.
- Predefined ACL queries on bucket insert/PATCH/PUT and object
  insert/copy/rewrite/compose fail with 400 when uniform access is enabled.
  Without uniform access they use the shared unsupported response.
- Schema 22 migrates existing buckets to disabled uniform access.
- Pinned Go and Ruby high-level clients create, inspect and disable access;
  Go Writer predefined ACL fails without publishing an object.
- Authorization at this original checkpoint was IAM-only. Subsequent ACL CRUD
  and selected-generation object read grants have separate [storage](json-acl.md)
  and [permission evidence](acl-permission-effects.md). Bucket/owner/membership,
  canned/default ACL effects and remaining ACL/UBLA/PAP interactions stay pending.

## Sources and local choices

- [Uniform access](https://docs.cloud.google.com/storage/docs/uniform-bucket-level-access)
- [Bucket fields](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets)
- [Bucket insert](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets/insert)
- [Bucket PATCH](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets/patch)
- [Bucket update](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets/update)
- [Object insert](https://docs.cloud.google.com/storage/docs/json_api/v1/objects/insert),
  [copy](https://docs.cloud.google.com/storage/docs/json_api/v1/objects/copy),
  [rewrite](https://docs.cloud.google.com/storage/docs/json_api/v1/objects/rewrite)
  and [compose](https://docs.cloud.google.com/storage/docs/json_api/v1/objects/compose)

Storage v1 Discovery revision `20261020`, fetched on the contract date, has SHA256
`eeb0831df501d131c9497534b2c3d130ea40f9fa215195115d011cb64e1a42bc`.
The official references specify 400 for predefined ACL parameters under UBLA.
The local reason is invalidArgument with guidance to remove the parameter and use
IAM. Without UBLA the shared unsupported response is 400 badRequest, as defined
by the integrated fail-fast slice; neither case applies an ACL.

The resource reference warns that supplying both aliases can be unreliable.
Locally the explicit modern enabled value takes precedence over the legacy
alias, matching Ruby's high-level setter, which resends the old alias from GET.
Use only uniformBucketLevelAccess in hand-written requests. Null/empty settings
are absent, and disabled output
contains enabled:false without lockedTime on both aliases. Output-only lockedTime
is ignored. Exact provider error text and disabled field omission need provider
evidence; these choices are tracked in SPEC-UBLA-EDGES. A null iamConfiguration
resource remains invalid; null uniform-access children mean absent input.
PATCH and PUT preserve omitted uniform access, including on locked buckets.
Disabling resets the deadline; subsequent activation starts another 90-day period.

## Persistence and authorization

Schema 23 adds one nullable microsecond deadline column transactionally. NULL
means disabled, so existing schema 22 buckets preserve their settings and IAM.
No body transfer or extra long-lived transaction is introduced. The catalog
checks update authority, metageneration and the lock deadline in its existing
short mutation transaction. Repeating enabled:true never extends the deadline.
Predefined ACL requests always fail before mutation/body publication; concurrent
toggles can affect error classification but cannot make an ACL request succeed.

The authorization engine already uses project/bucket IAM exclusively. UBLA
therefore preserves current grant results, verified by the existing strict
private-object journey before/after activation. PAP remains separately enforced.
Cloud creation's additional default IAM grants and ACL preservation/restoration
are pending alongside ACL CRUD. The initialization file still rejects uniform
access settings; this API slice does not change that closed input contract.

## Verification

Go Storage v1.70.0 / API v0.309.0 and Ruby google-cloud-storage 1.62.1 use their
ordinary high-level create/get/update calls. The added Go and Ruby cases failed
before the implementation on UBLA creation. Rust 1.99.1, edition 2024, and all
39 dependency/freshness inputs were verified CURRENT with
`cargo xtask dependencies check` on 2026-10-27; no dependency was added.

Targeted verification passed:

- `cargo check --workspace --all-targets --locked --offline`.
- `cargo test --lib --locked --offline uniform_access`: schema 22 migration,
  restart, metageneration conditions, enable/disable/re-enable and 90-day boundary.
- `cargo test --test auth_http --locked --offline uniform_access`: three cases,
  including all scoped ACL queries, encoded keys, state preservation, strict
  permission denial, aliases and Terraform-style PAP creation.
- The PAP metadata/fields/conditions/restart regression case passed.

`just check-sdk` passed the complete HTTP Go suite, including
TestBucketUniformAccess, and the Ruby migration/default and strict journeys.
Both Ruby profiles passed ruby-bucket-uniform-access using the ordinary setter.
`just check` passed formatting, clippy, cross-target checks, every workspace
unit/wire/doc test, generated-ledger verification and offline shell checks.
The integrated regional and fail-fast Go/Ruby cases also passed alongside UBLA.
Markdown lint reported zero issues in the changed user documentation.
Final verification was performed on 2026-10-27; evidence is consolidated here.
HTTPS SDK, container/guide journeys, cloud differential tests and benchmarks are
excluded from this slice's verification by the requested execution scope.
