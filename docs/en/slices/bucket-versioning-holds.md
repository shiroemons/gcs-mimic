# Bucket versioning and default event-based holds

<!-- markdownlint-configure-file {"MD013": false} -->

Contract checked on **2026-10-22** for BEHAVIOR-OBJECT-029 and
BEHAVIOR-BUCKET-012 within the `bucket-core-parity` delivery gate. This document
defines acceptance, not a second implementation-status ledger. Bucket setting
parsing and persistence belong to the bucket settings slice.

## Sources and provenance

- [Object Versioning](https://docs.cloud.google.com/storage/docs/object-versioning): live-to-noncurrent transitions and disabling versioning.
- [Objects: get](https://docs.cloud.google.com/storage/docs/json_api/v1/objects/get), [delete](https://docs.cloud.google.com/storage/docs/json_api/v1/objects/delete), [list](https://docs.cloud.google.com/storage/docs/json_api/v1/objects/list), [patch](https://docs.cloud.google.com/storage/docs/json_api/v1/objects/patch) and [update](https://docs.cloud.google.com/storage/docs/json_api/v1/objects/update): selected generations, permissions and listing order.
- [Object resource](https://docs.cloud.google.com/storage/docs/json_api/v1/objects): writable `eventBasedHold`, generation strings and noncurrent-only `timeDeleted`.
- [Object holds](https://docs.cloud.google.com/storage/docs/object-holds) and [using holds](https://docs.cloud.google.com/storage/docs/holding-objects): defaults apply to subsequently created objects; metadata remains editable while bytes are held.
- [JSON errors](https://docs.cloud.google.com/storage/docs/json_api/v1/status-codes): replacement/deletion blocked by an active hold returns **403 `objectUnderActiveHold`**. This differs from **403 `retentionPolicyNotMet`**, which is outside this slice.
- [Buckets: delete](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets/delete): both live and noncurrent objects make a bucket nonempty.
- [Discovery v1](https://storage.googleapis.com/$discovery/rest?version=v1), fetched on the check date: revision `20261013`, SHA-256 `1f74b7a93b3d2872cc332fc20dafae1657fc255930ce336618bea3d0b4d95ae0`. `generation` is an int64 string on get/delete/patch/update; `versions` is boolean; hold is boolean and deletion time is RFC 3339.

The pinned comparison is pinned reference emulator **v1.56.1**, commit
`896ece51e49eed0946089f3089f6343855c0d106`, cloned into `$TMPDIR` outside
the checkout. Source inspection, rather than execution of its test suite, found:

| Source | Git blob | Observed behavior |
| --- | --- | --- |
| fakestorage/object.go | `4dbe7e768b452a2b23d5407d4d2d660c8c7583ed` | Generation-selected reads/deletes; plain delete archives in versioned buckets. Listing sorts names only and uses name-only continuation. No object hold field or enforcement. |
| internal/backend/memory.go | `dc54cef73a6a6d6bf1ab27f4faae304301943e00` | Overwrite/plain delete preserves archived generations with deletion timestamps. Explicit generation deletion removes live or archived data. Disabling versioning retains archives. Default hold is bucket metadata without an object effect. |
| internal/backend/fs.go | `bcf071e37237b05cdbf1f11f1e3af24fcfb63304` | Enabling versioning fails; only current generation is stored. Default hold is bucket metadata without an object effect. |

The required scope includes the memory backend's version history and Google's
default-hold effects, with durable opaque blobs. Source omissions do not justify
accepting an ineffective hold setting. Google listing order is name ascending,
then generation **increasing**, as stated by the current list reference; the
comparison implementation does not establish that order or safe same-name paging.

## Request, response and state decisions

An overwrite in a versioned bucket makes the previous live generation noncurrent
and sets `timeDeleted`; a new immutable generation becomes live. Plain delete
only makes the live generation noncurrent. Neither transition reclaims its blob.
Unqualified reads and default listing select live versions only. Explicit
`generation` selects exactly that generation, even when no live version exists.
Explicit generation deletion permanently removes only that generation, including
when it is live, through the existing deletion journal. Disabling versioning
preserves history while subsequent unqualified replacements/deletions reclaim
their old live blobs. No older version is automatically promoted.

`versions=true` includes all retained generations and the live generation, ordered
by binary object name then increasing generation. Tokens include the generation
cursor and are bound to the versions flag and existing filters. Prefix, delimiter,
offset, page-size and trailing-delimiter behavior retain the current bounded
keyset contract. Versioning does not change IAM: read/list/delete/update require
their existing object permissions, and overwrite still requires create plus
delete, with checks in the mutation transaction.

New media, multipart and resumable-finalization objects inherit the current
bucket `defaultEventBasedHold`, unless object metadata explicitly supplies
`eventBasedHold`. Both boolean values are writable on insert, PATCH and PUT.
PATCH omission preserves the hold. PUT omission preserves the hold as a local
conservative rule pending independent evidence; explicit false releases it.
Invalid scalar/null input is rejected with 400 `invalidArgument`. Held live
objects reject overwrite and delete; a held noncurrent generation rejects explicit
deletion. Metadata changes, including hold release on a selected noncurrent
generation, remain permitted. Hold changes increment metageneration without
changing generation, bytes or checksums. Rendering includes true holds; false
representation is a local wire choice documented below.

The live/noncurrent change, preconditions, hold check, authorization and deletion
journal insertion share one short SQL transaction. All generations stay in the
object catalog so existing bounded blob-reference recovery includes history.
Blob publication precedes the SQL reference commit; precommit orphans are
reclaimed, while committed history survives restart. Deferred permanent deletion
uses the existing reference-aware unlink/recovery protocol.

## Acceptance cases and test mapping

Every test below is a required observable check. Execution results are recorded
after the table. These descriptive IDs are test names, not work numbers.

| Case | Required assertions | Test IDs |
| --- | --- | --- |
| Version overwrite | Previous generation and bytes remain; new live generation; `timeDeleted` only on history; ordinary list hides history | `versioned_overwrite_and_generation_reads` |
| Plain delete | Removes live visibility, retains generation and bytes; second plain delete 404; no promotion | `versioned_delete_and_permanent_generation_delete` |
| Explicit delete | Removes only the selected live/noncurrent generation and its blob; repeated/missing generation 404; conditions preserve state on failure | `versioned_delete_and_permanent_generation_delete` |
| Paging and boundaries | Increasing generation order, same-name pages, prefix/delimiter/offsets, token scope, invalid boolean/generation | `versioned_listing_pages_and_query_boundaries` |
| Disabled setting | Existing history survives disabled versioning; subsequent live replace/delete reclaim data; history-only bucket remains nonempty | `disabled_versioning_preserves_history_and_bucket_nonempty` |
| Atomic races | Conditional overwrite versus plain delete serializes; exactly one conditioned mutation wins; neither loses the previous generation | `versioned_overwrite_delete_race` |
| Strict permissions | Viewer can read/list selected history but cannot delete/update; creator cannot overwrite existing live data | `versioned_strict_permissions` |
| Restart and crash | Committed transitions/history persist; precommit rename recovers old live/history; permanent deletion recovers its journal without losing other generations | `versioned_crash_recovery_preserves_referenced_generations` |
| Migration | v7 and bucket-only v8 migrate to v9 without changing object metadata, bytes, allocator or bucket settings; repeat open is idempotent | `versioned_schema_migrations_preserve_objects` |
| Default holds | Media, multipart and resumable publication inherit true; explicit false overrides; changed default does not rewrite existing objects | `default_hold_applies_to_all_upload_modes` |
| Held mutations | Delete/overwrite fail with 403 `objectUnderActiveHold`; failed requests preserve bytes/generation/list state, including concurrent finalization | `held_objects_reject_delete_and_replacement` |
| Release and metadata | PATCH/PUT set/release with atomic conditions, unchanged bytes/generation, incremented metageneration; restart persists; selected history can be held/released | `event_hold_metadata_release_and_restart` |
| Hold permissions/input | Unauthorized read/update/delete denied; invalid boolean/null and unsupported temporary hold rejected without mutation | `event_hold_permissions_and_invalid_input` |
| Official Go versioning | Writer overwrite, `Generation(n)` read/delete, `Query{Versions:true}` list | `TestObjectVersioning` |
| Official Go holds | Inherited hold, rejected delete, `ObjectAttrsToUpdate.EventBasedHold=false` release and successful delete | `TestObjectEventBasedHold` |

## Local verification

Checked on 2026-10-22 with the pinned Rust 1.99.1 and committed dependency graph.
Every Rust acceptance case above is **PASS**: eight versioning HTTP tests and
four hold HTTP tests. The crash case uses actual process kills after publication
before catalog commit and after committed archival. A real generation-specific
HTTP DELETE also commits its catalog removal and journal entry while an injected
non-file blob path interrupts reclamation; the process is killed, the original
blob is restored and startup retries that real journal. The test verifies removed
metadata, reclaimed bytes, an empty journal and preservation of the other
generation, without fabricating catalog state or claiming an instruction-level
unlink barrier. Existing v1 and v3-v6 migrations and corruption/read-only rejection
tests also remain passing.

| Command | Result |
| --- | --- |
| `env -u RUSTUP_TOOLCHAIN cargo test --workspace --locked` | PASS, 369 tests, no failures or skips |
| `env -u RUSTUP_TOOLCHAIN cargo test --locked --test object_versioning_http` | PASS, 8 tests |
| `env -u RUSTUP_TOOLCHAIN cargo test --locked --test object_hold_http` | PASS, 4 tests, including explicit-false session restart and resumable version preservation |
| `env -u RUSTUP_TOOLCHAIN cargo test --locked --lib object_list_seeks` | PASS, live and versioned delimiter seeks remain below the existing 10,000 SQLite instruction cap over 20,001 objects |
| `env -u RUSTUP_TOOLCHAIN cargo test --locked --lib object_list_skips_noncurrent` | PASS, actual seek SQL plans use `objects_live` for live listing without sorting; plain/delimiter pages, history-only prefixes and versioned seeks stay within one 10,000-instruction budget with 20,000 noncurrent generations |
| `env -u RUSTUP_TOOLCHAIN cargo test --locked --lib object_list_tokens_reject` | PASS, correctly checksummed malformed cursors, versions scope and signed-int64 generation boundaries |
| `env -u RUSTUP_TOOLCHAIN cargo fmt --check` | PASS |
| `env -u RUSTUP_TOOLCHAIN cargo clippy --workspace --all-targets --locked --offline -- -D warnings` | PASS |
| `env -u RUSTUP_TOOLCHAIN cargo xtask dependencies check` | PASS, all compiler, direct library, SDK, Action and container inputs CURRENT; no dependency added |
| `npx --yes markdownlint-cli2 docs/en/slices/bucket-versioning-holds.md` | PASS |
| `GOTOOLCHAIN=auto go test -run '^$' ./...` from `sdk-tests/go` | PASS compilation |
| `just check-sdk` in native and compatibility profiles | PASS, both SDK acceptance tests plus the complete suite and Go vet; [raw output](../evidence/bucket-core-parity-sdk-go.txt) |

Both official Go cases are **PASS**, using Go 1.27.3, Storage v1.70.0 and
API v0.307.0 over HTTP JSON against an isolated local endpoint. The pinned
high-level `BucketAttrs` create conversion omits `DefaultEventBasedHold`, so
that bucket is created through the official generated JSON `Buckets.Insert`
client; the Writer, rejected delete and hold release use the high-level SDK.
The final integrated SDK suites rerun both cases after index-selection,
explicit-column migration, routing and profile-name changes. The final raw
record supersedes the narrow implementation-time SDK capture.

The object HTTP tests initialize bucket settings through the catalog schema so
they independently test object effects; bucket create/GET setting wire behavior
is verified by the separate bucket slice. Integrated
`bucket_api_settings_drive_versioning_and_hold_effects` now creates both true
settings exclusively through the bucket HTTP API in each compatibility profile
and verifies inheritance, blocked mutations, release, retained bytes/history,
history-only nonempty checks and permanent cleanup. It uses no direct SQLite
seeding. Final container, complete SDK and guide results belong to
[integration evidence](../evidence/bucket-core-parity.md); no real-cloud call or
performance benchmark is claimed.

## Exclusions and recorded differences

- Soft delete, retention policy, object retention, lifecycle, noncurrent-time
  conditions and temporary holds are out of scope. `temporaryHold` stays explicitly
  unsupported, including false; this is an emulator 501 rather than Google's error.
- Configuration propagation is immediate locally; Google's versioning guide
  advises allowing at least 30 seconds for configuration changes to propagate.
- Exact hold error message text, false-field omission versus explicit false,
  null and omitted PUT hold handling, combined failure precedence and timestamp
  changes on archival have not been verified against a real Google bucket.
  Error status/reason are sourced above; local choices must not be called exact
  provider wire parity. No real-cloud test or irreversible retention action runs.
- Existing precondition/authorization ordering, unknown/output-only input rules,
  retry-token replay and opaque local ETags/URLs retain the recorded deviations
  in `compatibility/gaps.json`; this slice does not claim to close them.
- Generation-specific DELETE condition selection remains an unverified provider
  interaction. Rechecked on **2026-10-22**: the
  [delete reference](https://docs.cloud.google.com/storage/docs/json_api/v1/objects/delete)
  selects a revision with `generation`, but describes conditions using current
  generation/metageneration and live-version wording. The
  [preconditions guide](https://docs.cloud.google.com/storage/docs/request-preconditions)
  instead describes the target resource and allows explicitly selected noncurrent
  versions. Locally all four conditions compare with the selected generation,
  including its own metageneration, even when another live version exists or no
  live version remains. This interpretation is consistent with targeting an
  explicit revision, but the method-specific wording does not settle every
  combination, particularly not-match with no live version. The negative and
  successful selected-generation cases in
  `versioned_delete_and_permanent_generation_delete` preserve this rule; exact
  provider behavior needs authoritative clarification or authorized differential
  verification before the integration ledger can claim this gap closed.
- Compared with the pinned source, persistent history, Google generation ordering,
  generation-aware paging and actual holds intentionally provide stronger effects.
  The source backend difference is explicit; a disk-backed emulator must not lose
  history merely because the comparison filesystem backend lacks it.
- Selected process-kill boundaries establish local recovery only. Host power loss,
  every individual fsync/unlink window and cross-platform durability are unverified.
- Schema migration covers supported v1 and v3 through v8 catalogs. Unreleased
  v2 remains rejected before mutation because its deleted-generation high-water
  information is unavailable, preserving the existing fail-closed policy.
