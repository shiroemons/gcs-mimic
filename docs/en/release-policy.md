# Versioning and licensing policy

[日本語](../ja/release-policy.md)

## Product version and compatibility

gcs-mimic follows [Semantic Versioning 2.0.0](https://semver.org/), with the
additional compatibility rules below during initial development. The root
`Cargo.toml` package version is the product version and supplies `--version`.
The latest release is `0.7.0`; a manifest version alone
does not establish that a release exists. The internal xtask package is not a
separately released product.

The product version is independent of Google API versions, SDK versions and the
catalog schema version. The [feature ledger](../../compatibility/features.json)
defines the supported API subset; a product version does not claim complete
Google Cloud Storage compatibility.

During `0.x` development, use these rules:

| Change | Version update | Example |
| --- | --- | --- |
| Compatible bug or security fix | Patch | `0.1.0` to `0.1.1` |
| Feature or deprecation | Minor; reset patch to zero | `0.1.1` to `0.2.0` |
| Breaking change | Minor; reset patch to zero | `0.2.1` to `0.3.0` |

This is a project promise stricter than SemVer's general allowance for unstable
`0.x` APIs. A patch release must preserve the documented contract. Judge dependency
and container-base updates by their observable effects using the same rules.
A correction to match Google behavior is still a breaking change if it requires
users to change a documented integration or migrate persistent data.

Compatibility covers documented CLI commands and options, environment variables,
configuration formats and defaults, supported API behavior, supported runtime
platforms, and access to persistent data. Internal functions, implementation
details and human-readable diagnostic wording are not stable interfaces.

Document breaking changes and migration steps in release notes. Catalog schema
numbers are separate from application versions; schema changes that preserve the
documented upgrade path need not be breaking. Never silently reset or discard an
existing data directory to make an upgrade work. Document required migrations,
backup steps and downgrade restrictions; downgrade compatibility is not assumed.

## Progress through 0.x

Release useful increments while compatibility work continues. Do not keep adding
released functionality under `0.1.0` until the whole roadmap is complete.
For example, after the first `0.1.0` release, a bucket initializer can ship as
`0.2.0`, a compatible correction as `0.2.1`, and directory synchronization as
`0.3.0`. These are examples, not reserved feature versions or dates. Seed
extensions and data import can each ship in another minor release; neither must
wait for synchronization. Parallel work is assigned versions at integration.

Choose the next version from the last published release and its actual changes.
Use the highest required increment when combining changes. Documentation-only
edits do not require a product release. Update the application manifest and
lockfile, user examples, release notes and artifact references together when
preparing the chosen release. A local preview image is not a published version.

The current migration additions are unreleased source, even while the package
version still reads 0.7.0. Their final SDK/container/browser gates are recorded
in [migration integration](evidence/migration-integration.md). Atomic seed replay
changes the previous per-file protection skip: a differing held/retained fixture
now rejects the whole replay and prevents readiness. Document that migration
explicitly and use a minor release under the rules above; never assign a patch
or imply that the existing 0.7.0 image includes it. Scoped Go subprocess adapters,
local Pub/Sub and RAM storage do not by themselves close the 1.0.0 gate.

Unreleased user-visible changes that the next minor release notes must state:

- Creating a bucket notification configuration now checks that the Pub/Sub topic
  exists when a local Pub/Sub emulator is configured (`GCS_MIMIC_PUBSUB_EMULATOR_HOST`
  or `PUBSUB_EMULATOR_HOST`). A missing topic returns Cloud Storage's 400; an
  emulator that cannot be reached returns a retryable 503 and creates nothing.
  Without an emulator, topics are still accepted unchecked.
- A new native bucket created without `softDeletePolicy` now gets Cloud Storage's
  seven-day soft delete, so deleted objects and buckets keep their bytes for
  seven days. This includes overwriting an object without versioning: the
  replaced generation is retained for seven days too. Set
  `GCS_MIMIC_DEFAULT_SOFT_DELETE_RETENTION_SECONDS=0` (or
  `--default-soft-delete-retention-seconds 0`) to keep permanent deletion.
  Existing buckets and the legacy-emulator profile are unchanged, and
  `--backend memory` defaults to 0 unless the setting is given.
- In the native permissive profile (the default), objects now record a local
  creator as `owner` and as a `user-` OWNER ACL entry, as Cloud Storage records
  the calling user: by default `user-local-user@gcs-mimic.invalid`, listed after
  the project teams. Copy and rewrite responses name the owner even without
  `projection=full`. Change the address with `GCS_MIMIC_LOCAL_CREATOR_EMAIL` or
  `--local-creator-email`, or set it empty to keep `project-owners-N`. The creator
  only labels who wrote the object and never authorizes. Existing objects keep
  working: those owned by the project owners stay so, and the creator name follows
  the setting at the next start. The legacy-emulator profile is unchanged, and
  strict mode already recorded the registered caller. In strict mode, copy
  and rewrite responses now also name the registered caller as `owner` (without
  `acl`), and the reserved identity id namespace `gcs-mimic:` is rejected in
  `identity_file`. An explicit `local_creator_email` is rejected in strict mode
  and in the legacy profile. Clients that compared an object ACL or `owner` with
  `project-owners-N` alone must expect the extra entry.
- Native bucket resources carry `satisfiesPZI: true`, nanosecond-scale
  19-digit `generation` values (schema 46 rescales stored ones) and
  millisecond timestamps; JSON media responses no longer send `accept-ranges`.
- A bucket insert, PATCH or PUT body that carries `satisfiesPZS` no longer gets
  400; the member is ignored and never emitted, as Cloud Storage did. A bucket
  PATCH that carries no writable setting (for example `{}` or only
  `satisfiesPZI`) no longer advances `metageneration` or `updated`.
- Object retention follows Cloud Storage more closely. `retainUntilTime` is
  compared to the nanosecond and displayed truncated to the millisecond, so
  writing back the displayed millisecond value is a shortening. Shortening or
  removing an Unlocked retention, or locking it, without
  `overrideUnlockedRetention=true` returns 403 `forbidden` (gRPC
  `PERMISSION_DENIED`) instead of 400 `invalidArgument`. A Locked retention
  refuses shortening, removal and a mode change with 403 `forbidden` and the
  message `The locked object retention cannot be removed. Its retention mode
  cannot be changed and its retention period cannot be shortened.`, with or
  without the override; sending the same time again is accepted. Deleting a
  retained object returns 403 `retentionPolicyNotMet` naming the object and
  the retain-until time in US Pacific time, and `objects.delete` ignores
  `overrideUnlockedRetention`. gRPC returns `retention_expire_time` at
  millisecond precision, and the JSON `fields=retentionExpireTime` is
  rejected with 400 `invalidParameter`.
- Catalog schema 47 adds `objects.retain_until_sub_micro_nanos` for the
  nanosecond remainder. The migration is automatic and one-way like the
  earlier ones (the log names the versions); rows stored by 0.7.0 (whole
  microseconds) keep their time with a zero remainder.
- `temporaryHold` and `eventBasedHold` are now reported as `false` once they
  were set and released, and omitted on an object that never had them, as
  Cloud Storage does. A PATCH or PUT that names a hold sets it to the given
  value (also `false`); a PUT that omits it returns it to omitted; a create that
  names `false` returns `false` (the recording has no create-time `false`, so
  this follows the PATCH behavior); a copy, rewrite or compose destination
  starts omitted. Only `true` blocks delete and
  overwrite. gRPC keeps `event_based_hold` always present and `temporary_hold`
  a plain bool. Catalog schema 48 adds `objects.temporary_hold_released` and
  `objects.event_based_hold_released`; the migration is automatic and one-way
  like the earlier ones. Rows stored before it read as omitted (0) or `true`
  (1): a stored 0 cannot show whether the hold was ever set, so no `false` is
  invented.
- A bucket PATCH that carries `autoclass:{}` is a no-op like `{}`. An empty
  PATCH answers 304 (no body) when `ifMetagenerationNotMatch` names the current
  value and a native-profile 412 (`At least one of the pre-conditions you
  specified did not hold.`, location `If-Match`) when `ifMetagenerationMatch`
  does not match. A bucket PATCH that carries a writable setting now also
  answers 304 when `ifMetagenerationNotMatch` names the current value.
- A bucket retention policy now refuses to delete or overwrite an object with
  the same 403 `retentionPolicyNotMet` message as object retention
  (`Object '<bucket>/<name>' is subject to bucket's retention policy or object
  retention and cannot be deleted or overwritten until <time>`), naming the
  creation time plus the period in US Pacific time to the microsecond. The
  gRPC `DeleteObject` returns `PERMISSION_DENIED` with that message.
- gRPC `Bucket.project` is `projects/{project number}`, like JSON
  `projectNumber` (a numeric project ID verbatim, `0` otherwise), instead of
  `projects/{project ID}`, because SDKs parse it as a number. gRPC read masks accept
  `checksums.crc32c` and `checksums.md5_hash` (only that digest is returned).
  A gRPC request that passes `projects/0` (what a project without a number
  renders as) as a bucket project or parent is rejected with
  `INVALID_ARGUMENT`.
- JSON API responses carry Cloud Storage's common headers: `cache-control`,
  `expires`, `vary: Origin, X-Origin`, `pragma: no-cache` on mutations and
  errors, `content-type: application/json` on 204 and 304, and a bare `ETag`
  header equal to the body `etag` (not on Move; the IAM header is unquoted and
  `If-Match` accepts the bare value). XML virtual-host responses do not get them.
  The upload path follows the same rules: the resumable initiation reply and
  the upload errors carry `cache-control`, `expires` and `pragma`, and
  `alt=media` downloads and media failures carry the `expires` their cache
  policy implies. A session request the upload front end rejects (a malformed
  `Content-Range`, a length, total or offset that disagrees with it, an
  unknown, finished or cancelled session) answers with a bare
  `text/plain` body and without `cache-control`, `expires`, `pragma` or
  `vary`, as does a session status reply (308) and a cancellation (499); a
  client that read these bodies as JSON must read the text. Checksum and
  header errors on a session request keep their JSON error. Upload errors
  that carry a JSON envelope keep
  `x-content-type-options: nosniff`, which Cloud Storage does not send, because
  the body quotes request input under a `text/html` content type. JSON error
  bodies add `location` and `locationType` (the parameter name, or `If-Match`
  for a failed precondition).
- Bucket errors use reason `invalid` and Cloud Storage's wording (for example
  `The specified bucket does not exist.`). Bucket PATCH defaults to
  `projection=full`; PUT answers 400 when the body `name` differs from the path.
  `predefinedAcl` applies on PATCH and PUT. A different `location` is accepted
  and a non-map `labels` clears all labels. `setIamPolicy` advances
  `metageneration` and drops bindings with no members.
- Three bucket behaviors change in the native profile only; the
  legacy-emulator profile keeps its earlier behavior. A bucket PUT replaces the
  resource: it clears omitted `labels`, `cors`, `lifecycle`, `versioning`,
  `retentionPolicy` and `defaultEventBasedHold` (a locked retention policy
  stays) but keeps the storage class, the soft delete policy and the access
  settings it omits, where it used to preserve every omitted setting. A client
  that sends PUT for an update, such as the Go `Buckets.Update` call or an SDK
  update helper built on PUT, therefore loses the settings it leaves out. A
  bucket method ignores an undeclared query parameter (for example the
  `append=false` that Node.js `addLifecycleRule` sends) instead of answering
  400, and an array-valued `lifecycle` is accepted. Object methods still
  answer 400 for an undeclared query parameter in both profiles.
- JSON media uploads no longer verify `X-Goog-Hash` or `Content-MD5` (as
  recorded from Cloud Storage); multipart metadata, resumable and XML uploads
  still do. A request without `uploadType`, or a non-multipart body, is a media
  upload. Unknown metadata members and mistyped holds are ignored; PUT clears
  holds it does not name. A JSON media (`alt=media`) download ignores
  `If-Range` and the date validators, while a metadata or bucket GET that
  carries one of them still answers 400 `badRequest` (a local choice). A
  multi-range request answers 501.
- Several earlier successes and failures are reversed:
  a multipart upload whose metadata `contentType` disagrees with the media
  part's content type answers 400 (the metadata type used to win with 200); a
  resumable session evaluates the preconditions given at start (such as
  `ifGenerationMatch`) when the upload completes, not at start; the JSON
  `fields` parameter accepts empty segments, a trailing comma and an empty
  value (they used to answer 400); an XML `DELETE` rejects an unknown
  `x-goog-*` header; an ACL request that names a project-team entity of
  another project answers 400 `Project "N" was not found.` (any other missing
  entity answers 404 `The specified key does not exist.`); the object PUT
  response uses the `noAcl` projection; `retentionPolicy.isLocked` is omitted
  while the policy is unlocked; and a bucket with uniform bucket-level access
  hides the `owner` of its resources.
- XML: list element order and `MaxKeys`, `Vary`/`Expires`, the PUT
  `Content-Type`, 412 wording and copy/DELETE `Details` follow Cloud Storage; a
  `x-goog-copy-source-if-match` mismatch and a multi-range GET answer 400;
  object GET and HEAD ignore unknown query parameters and `x-goog-*` headers
  (known but unsupported ones such as `userProject` are rejected).
- gRPC errors carry Cloud Storage's messages and details (`ErrorInfo`,
  `BadRequest`); object create and update times are millisecond precision,
  `WriteObject` and `UpdateObject` return `acl` and `owner`, `ReadObject` no
  longer ends with an empty message, and an unknown `read_mask` path is
  `INVALID_ARGUMENT`.
- Object retention times are nanosecond `UnixNanos` values on every OS. The
  Windows floor to 100 ns ticks still awaits confirmation on Windows CI.

## The 1.0.0 gate

`1.0.0` means a verified functional superset of the **pinned fake-gcs-server
baseline**, with a stable public gcs-mimic contract. All of these must pass:

1. Close the [full reference gate](fake-gcs-server-migration.md#delivery-and-full-superset-gate)
   for its complete capability set: configuration behavior, JSON/XML, backend
   modes, TLS, helper/event behavior, Go integration and Storage v1 gRPC.
   Test both reference backends and representative existing applications.
   Different setting names and explicit deployment/data-conversion steps are
   allowed with tested mappings; unchanged CLI spelling is not required.
2. Verify preservation of source-visible data and settings, including exposed
   history and identities, safe cutover/recovery and rollback. Missing source
   semantics cannot be waived by narrowing a comparison row.
3. Verify the named additions: independent per-bucket settings, a create-missing
   bucket initializer with API updates preserved, bidirectional directory
   synchronization, strict local authorization and V4 RSA validation, and
   persistent version history. Keep these scopes fixed in the release contract;
   unrelated future ideas do not silently become new blockers.
4. Pass the release's platform, SDK/browser, resource, persistence and public
   distribution gates. Record every baseline capability and added scope with
   executable evidence, publish migration guidance, and commit to the versioned
   public contract.

The current reference is fake-gcs-server **v1.56.1** at the pinned source revision
in the [migration guide](fake-gcs-server-migration.md). Refresh the upstream
inventory before release; explicitly record the compared version and any newer
baseline adoption. A `1.0.0` release does not assert compatibility with untested
upstream versions. Unsupported reference behavior remains a blocker, including
behavior that needs a separate legacy profile rather than native strict rules.

Full Google Cloud Storage API coverage is a separate goal, not this version's
definition. After `1.0.0`, breaking public-contract changes increment major,
compatible additions increment minor, and compatible fixes increment patch.

## Release identity and publication

Choose the product version when preparing a release, not for every commit or
push. Git release tags use `vX.Y.Z`; binary metadata and the exact container version
tag use `X.Y.Z`. They must refer to the same product version and source commit.

Published release tags and artifacts are immutable: do not move a release tag,
replace a binary, or overwrite an exact container version tag. Publish corrections
or security rebuilds under a new version, even if only dependencies or the base
image changed. Record artifact checksums and image digests with each release.

The first general developer release retains the
[roadmap's P1 plus essential P2 public/private and URL target](roadmap.md#first-useful-release).
Before publication, update the version and lockfile together, reconcile the
feature ledger and documentation, run dependency freshness and pre-push checks,
and verify the release's supported platforms in CI. Follow the existing
[dependency release gates](dependency-policy.md#ci-lanes). Release notes describe
the final changes, compatibility impact, migration instructions, tested platforms
and known limitations.

The [native distribution procedure](binary-distribution.md) defines the
archive targets, manual verification and future tag-driven GitHub Release flow.
Tag publication requires matching Cargo versions, committed release notes,
freshness of the release inputs (see the
[dependency policy](dependency-policy.md#ci-lanes)), Foundation checks and all
native suites. The
[GHCR procedure](container-distribution.md) adds native image checks, immutable
version tags, advancing minor/latest aliases and digest reports after binary
Release creation. Configured workflows do not establish that a public
release or successful all-platform execution has occurred.

## License and redistribution

Copyright (c) 2026 shiroemons

Unless explicitly noted otherwise, original project code and documentation are
available under either of these licenses, at the recipient's option:

- [MIT License](../../LICENSE-MIT)
- [Apache License, Version 2.0](../../LICENSE-APACHE)

The SPDX expression is `MIT OR Apache-2.0`. A recipient chooses either license;
compliance with both at once is not required. Cargo declares this expression in
`workspace.package.license`, inherited by the application and xtask.

Third-party code, dependencies, schemas and other source material retain their own
licenses. This project's dual-license declaration does not relicense those works.
Binary archives and container distributions must include both project license
texts and the applicable third-party license and attribution notices, including
any required NOTICE content. Review the actual shipped dependency graph and
container contents when implementing distribution packaging.
