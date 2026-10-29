# Roadmap

<!-- markdownlint-configure-file {"MD013": {"tables": false}} -->

[日本語](../ja/roadmap.md)

Updated **2026-10-29** against the `0.4.1` source, tests and recorded evidence.
Continue closing the gaps in existing fake-gcs-server workflows through useful
`0.x` increments; reserve **1.0.0** for verified reference compatibility plus
the named extensions. The order below selects work, not release dates.

## Available today

The [published 0.4.1 release](https://github.com/shiroemons/gcs-mimic/releases/tag/v0.4.1)
provides native archives and Linux AMD64/ARM64 images on GHCR and Docker Hub.
The HTTP quick start, HTTPS default, simultaneous HTTP/HTTPS listeners,
public-host routing, default bucket location and log-level controls are available.
The [configuration mapping](fake-gcs-server-configuration.md) documents native
equivalents and diagnostics for all 19 known source environment variables.

Scoped workflows include multiple buckets, per-bucket CORS/IAM, streaming
transfers, rewrite/compose/move, version history, holds, bucket/object retention,
soft delete/restore, V2/V4 RSA signed transfers and V4 signed POST policies.
JSON ACL resources, notification configurations, HMAC key management and
Autoclass settings also exist, with the effect limitations below.
Storage v2 gRPC includes resumable and appendable writes and ranged reads;
legacy Storage v1 serves the twelve inventoried reference RPCs with field limits.
See the [0.4.0 feature scope and limits](releases/0.4.0.md).

Bucket initialization creates missing buckets and preserves API changes.
Seed files are applied on every startup and use extension-derived MIME types.
They replace differing objects at the same names, subject to holds and retention;
identical objects keep their generation. A sync directory mirrors API changes
and ingests external creates/edits at startup or an internal rescan.
The importer converts supported live filesystem snapshots with metadata
and source identities.
See [container usage](container-usage.md) and the
[migration guide](fake-gcs-server-migration.md).

Go/Ruby SDK and browser evidence covers declared subsets. Rails Active Storage
has a [working application credential recipe](slices/sdk-local-auth-softdelete-notes.md),
but default `storage.yml` authentication still attempts external OAuth and fails
the offline scenario. These are not whole-API compatibility claims. See the
[comparison](feature-comparison.md) and [feature matrix](feature-matrix.md).

## Missing capabilities, in priority order

Priority selects work when capacity is limited. Only an explicit prerequisite
blocks another track; a higher-priority row does not make every later row wait.

| Priority | User outcome | Prerequisite | Can run alongside |
| --- | --- | --- | --- |
| First | Observe external file creates/edits continuously | Bounded watcher/rescan contract | Import and reference coverage |
| Next | Complete safe bidirectional synchronization | Deletion approval, conflicts and recovery | Independent client/protocol work |
| Next | Preserve the remaining source state on migration | Source history, classes and cutover contract | Directory sync |
| Then | Close reference behavior and ordinary Rails setup | Scoped API/profile and credential contracts | Independent reference capabilities |
| Every release | Reconcile evidence and pass distribution gates | Platform, dependency and artifact verification | All implementation tracks |
| Final gate | Release the verified superset as 1.0.0 | Complete baseline and named extensions | Combine verified scopes |

The next proposed slice is continuous observation of external creates/edits,
reusing the existing [ingestion/rescan path](../../src/sync/ingest.rs).
Define bounded event handling, missed-event reconciliation, partial-write settling
and shutdown cases before implementation. Preserve the current no-external-delete
and conflict-preservation rules; do not bundle deletion or strict sync into it.
DESIGN-062, DESIGN-072 and DESIGN-074 remain `PARTIAL`.

Further sync work includes external deletion and rename removal, deletion approval,
conflict transfer/control, interrupted write-back recovery and platform guarantees.
The [sync contract](slices/directory-synchronization.md) separates these pending
cases from continuous API export and existing external create/edit ingestion.

Import is no longer an unimplemented track. Its remaining scope includes archived
or memory-backend history, original names lost by source normalization, source
IAM/settings and safe cutover/rollback. Filesystem publication currently rejects
non-`STANDARD` source object classes. Seed metadata sidecars remain unsupported.
See the [import contract](slices/import-filesystem.md).

Remaining reference work includes JSON/XML field and wire differences, HTTP batch,
helper/reset/reseed routes, event publication, embedded Go and the memory backend.
Existing legacy v1 RPCs still need full reference input/response coverage.
ACL entries are stored and inherited but do not authorize object access;
notification configurations do not publish events. HMAC keys do not authenticate
requests. Autoclass automatic transitions and lifecycle action execution remain
absent; customer-supplied keys enforce reads without encrypting local disk bytes.
Broader Google behavior is separate from the pinned-reference minimum.
Source inventory determines the final reference set; these examples do not exclude
an unlisted source capability.

## Parallel development boundaries

Watcher, import and reference work can proceed independently where their own
prerequisites are satisfied. Preserve the existing bucket identity, local
administrative authority and startup/readiness ordering. Sync starts after ready;
ready does not mean that its initial reconciliation has finished.

The initializer owns file parsing and create-missing orchestration. The sync
adapter owns watching, rescans and write-back. The importer owns read-only source
decoding and preservation reports. Shared catalog/publication changes have one
integration owner; adapters must not bypass conditions, hashes or generations.
Existing SDK/browser checks can run throughout, without waiting for these adapters.

See the [detailed plan](delivery-plan.md#parallel-work-and-acceptance) for
dependencies, acceptance cases and ownership boundaries.

## Agreed behavior and open decisions

The bucket initialization file creates **missing buckets only**. Restart keeps
existing settings and objects, including API changes. Removing a file entry
does not delete a bucket. Its [version-1 schema and local administrative authority](slices/bucket-initialization.md)
are implemented; IAM policy bootstrap remains outside that schema.

Directory sync must handle API/file conflicts, unusual names, partial writes,
missed events and recovery. The delivered subset leaves conflicting files in
place; fix the remaining deletion and conflict-transfer policies before
implementing them. Sync currently requires permissive authentication; strict
original-owner authorization remains pending. A permanent write-back I/O error
can delay graceful shutdown until space or permissions are repaired.
SQLite remains the catalog; monitoring files and updating only their sizes is
not sufficient. See the [filesystem requirements](fake-gcs-server-storage.md#required-gcs-mimic-follow-up).

## Releases

Publish feature additions as successive `0.x` minor releases and compatible
fixes as patches. Do not wait for complete parity while staying at `0.1.0`.
Initializer, sync and import can ship separately; version numbers are assigned
to actual release scopes rather than reserved for tracks.

`1.0.0` requires complete pinned-reference functionality, tested migration and
the specified extensions. Full Google API coverage is separate. See the
[version policy and exact gate](release-policy.md#the-100-gate).

## First useful release

The first useful `0.x` release has shipped; `0.4.1` is the current published
baseline. The original gate covered scoped JSON, public/private access and URL
workflows, persistent storage, fixtures, Go/Ruby, CORS and local HTTPS.
Rails, lifecycle, gRPC and full parity were not prerequisites for that first release.

Native CI/platform checks, current dependencies, release notes, packaged
notices/checksums and verified public image access still gate each release.
The DIST-001 through DIST-005 ledger rows remain `PARTIAL`; published artifacts
do not close their remaining acceptance or evidence-reconciliation work.
Local checks alone do not complete the [distribution gates](container-distribution.md).

## Beyond reference compatibility

Continue broader Google coverage in independently verified slices: remaining
XML and Storage v2 gRPC fields/operations, additional SDKs, advanced policy,
retention and lifecycle effects, hierarchical storage, full Rapid bucket
semantics and service simulations. Existing appendable-object support does not
reproduce zonal Rapid storage. These remain in the
[detailed coverage plan](delivery-plan.md#coverage-retained-for-later-delivery).

The [feature ledger](../../compatibility/features.json) owns implementation
status. This roadmap sets priorities without promoting planned work to supported
behavior or changing existing feature IDs.
