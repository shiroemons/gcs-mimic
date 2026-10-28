# Roadmap

<!-- markdownlint-configure-file {"MD013": {"tables": false}} -->

[日本語](../ja/roadmap.md)

First make gcs-mimic easy to run from a Docker image and use with existing
fake-gcs-server workflows. Deliver missing capabilities in useful increments;
reserve **1.0.0** for verified reference compatibility plus the named extensions.

## Available today

The current scoped workflows include multiple buckets, per-bucket CORS/IAM,
object transfers and version history, persistent storage, one-time seed import,
HTTP/HTTPS and V4 RSA signed transfers. Go/Ruby SDK and browser evidence exists
for the documented subsets. These are not whole-API compatibility claims.
See the [comparison](feature-comparison.md) and [feature matrix](feature-matrix.md).

## Missing capabilities, in priority order

Priority selects work when capacity is limited. Only an explicit prerequisite
blocks another track; a higher-priority row does not make every later row wait.

| Priority | User outcome | Prerequisite | Can run alongside |
| --- | --- | --- | --- |
| First | Pull an image and finish HTTP quick start | Release/platform gates and authorized publication | All implementation tracks |
| First | Translate familiar CLI/environment settings and public-host behavior | Pinned option, URL and routing contracts | Image preparation and storage design |
| Next | Edit bucket/object files and see changes through the API in both directions | Conflict/name/recovery design and shared mutation contract | Initializer and import |
| Next | Preserve fixture MIME/metadata and import existing filesystem data | Pinned source format, preservation and recovery contract | Initializer and directory sync |
| Then | Run Rails and close remaining reference capabilities | Each API/profile contract; existing prerequisites in the detailed plan | Independent clients/protocols |
| Final gate | Release a verified functional superset as 1.0.0 | Complete baseline, migration, named additions and release evidence | Gate combines completed tracks |

Public image publication is pending; local ARM64 and emulated AMD64 images pass
the [onboarding checks](evidence/container-onboarding.md). Version-1 initialization
files create missing buckets and preserve API changes. A sync directory mirrors
API writes and imports external creates and edits at startup; external deletion,
a file watcher, conflict transfer and source-state import are not implemented.

Configuration work covers all [19 source environment variables](fake-gcs-server-configuration.md).
Missing behavior includes combined public-host/routing configuration, dual
listeners, memory-mode ergonomics, notification settings, global location and
log-level controls. Literal option-name equality is not required. Equivalent
functionality still needs evidence before a compatibility claim.

Remaining reference work includes ACL/metadata and wire differences, batch,
helpers/events, Storage **v1** gRPC, embedded Go and both backend workflows.
Source inventory determines the final set; these examples do not exclude an
unlisted source capability. Rails integration is another required user journey.

## Parallel development boundaries

Distribution and configuration work can start independently. Agree on bucket
identity, initialization permissions, startup/readiness ordering and the shared
mutation interface before writing the initializer and sync adapters.

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
implementing them.
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

Ship the declared JSON, public/private access and URL workflows with persistent
storage, fixtures, Go/Ruby checks, CORS and local HTTPS evidence. Rails, lifecycle,
gRPC and full reference parity are not prerequisites for the first `0.x` release.

Required native CI/platform checks, current dependencies, release notes, packaged
notices/checksums and verified public image access still gate publication.
Local image checks do not complete those [distribution gates](container-distribution.md).

## Beyond reference compatibility

Continue broader Google coverage in independently verified slices: remaining
XML and Storage v2 gRPC, additional SDKs, advanced policy/retention/lifecycle,
hierarchical storage, Rapid and service simulations. These remain in the
[detailed coverage plan](delivery-plan.md#coverage-retained-for-later-delivery).

The [feature ledger](../../compatibility/features.json) owns implementation
status. This roadmap sets priorities without promoting planned work to supported
behavior or changing existing feature IDs.
