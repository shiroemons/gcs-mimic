# Roadmap

[日本語](../ja/roadmap.md)

Updated **2026-11-10** for the 0.17.1 release. Published installation examples use `0.17.1`. The scoped SDK, browser and distribution results of the 0.8.0 migration additions are recorded in the [combined verification](evidence/migration-integration.md); they do not complete the broader feature rows.
Continue closing the gaps in existing fake-gcs-server workflows through useful `0.x` increments.
**1.0.0** freezes the public contract and requires tested data directory upgrades from every supported release; the complete fake-gcs-server superset remains a separate goal.
The order below selects work, not release dates.

## Available today

The [0.17.0 release](releases/0.17.0.md) provides native archives and Linux AMD64/ARM64 images on GHCR and Docker Hub. The HTTP quick start, HTTPS default, simultaneous HTTP/HTTPS listeners, public-host routing, default bucket location and log-level controls are available. The [configuration mapping](fake-gcs-server-configuration.md) documents native equivalents and diagnostics for all 19 known source environment variables.

Scoped workflows include multiple buckets, per-bucket CORS/IAM, streaming transfers, rewrite/compose/move, version history, holds, bucket/object retention, soft delete/restore, V2/V4 RSA signed transfers and V4 signed POST policies. JSON ACL resources, notification configurations, HMAC key management and Autoclass settings also exist, with the effect limitations below. Bucket and object PATCH/PUT replace ACLs atomically with other metadata. Media, multipart and copy invocation tokens replay the original response within bounded local retention, and JSON media GET and XML GET expand gzip-encoded objects. Storage v2 gRPC includes resumable and appendable writes and ranged reads; legacy Storage v1 serves the twelve inventoried reference RPCs with field limits. Since 0.9.0, `gcloud storage` runs the [example workflow](../../examples/gcloud/README.md), and [examples](../../examples/README.md) cover eight official clients. Since 0.10.0, Rails Active Storage 8.1.4.1 direct uploads work through `x-goog-content-length-range` enforcement, and XML reads evaluate conditional headers. See the [0.17.1 release notes](releases/0.17.1.md), the [0.16.0 release notes](releases/0.16.0.md), the [0.15.0 release notes](releases/0.15.0.md), the [0.14.0 release notes](releases/0.14.0.md), the [0.13.0 release notes](releases/0.13.0.md), the [0.12.0 release notes](releases/0.12.0.md), the [0.11.0 release notes](releases/0.11.0.md), the [0.10.0 release notes](releases/0.10.0.md), the [0.9.0 release notes](releases/0.9.0.md), the [0.8.0 release notes](releases/0.8.0.md), the [0.5.0 release notes](releases/0.5.0.md) and the [0.4.0 feature scope and limits](releases/0.4.0.md).

Bucket initialization creates missing buckets with their settings and
preserves API changes. Since 0.14.0, an entry can also give its bucket an
initial IAM policy, the document can be written inline in `GCS_MIMIC_BUCKETS`,
and reconcile mode, the default for the inline document, writes edited entries
to existing buckets. Seed files replay on every startup with extension-derived MIME types. Since
0.8.0 the replay prepares the complete tree and publishes it atomically; a
changed protected fixture fails the replay instead of skipping that file.
Identical objects keep their generation. A sync directory mirrors API changes
and ingests external creates/edits; continuous observation, revision-bound
deletion and recovery controls extend that path with explicit limits.
The importer converts scoped filesystem snapshots, while a separate Go capture
and snapshot reader preserve declared memory history and original names.
See [container usage](container-usage.md) and the
[migration guide](fake-gcs-server-migration.md).

SDK and browser evidence covers declared subsets. The
[eight-SDK scorecard](../../sdk-scorecard/README.md) passes every Go, Ruby,
Node.js, PHP, Python, Java, .NET and Rust case in its latest
[run](../../sdk-scorecard/results/2026-10-31-2/summary.md); scope and
remaining profiles are tracked in [SDK integration](slices/sdk-scorecard-integration.md).
Rails Active Storage has a [working storage.yml recipe](rails-active-storage.md)
with a non-Google universe. Since 0.8.0 it also verifies registered local
JWTs and supplies final-origin signing adapters; default Google-universe
OAuth still requires Google and is not a local credential recipe. Since 0.14.0,
the recipe also runs in strict mode with the built-in test signer's key, which
needs no registration; the strict Rails suite passes with the signer holding only
`roles/storage.objectUser`.
These are not whole-API compatibility claims. See the
[comparison](feature-comparison.md) and [feature matrix](feature-matrix.md).

Production-like local access needs 0.14.0 or later. In strict
mode, a trusted listener gives the application's own servers a port where their
credential-free clients, such as Go storage with `STORAGE_EMULATOR_HOST`, act
as one identity, while browsers stay anonymous and reach private buckets only
through signed URLs. The built-in test signer signs those URLs with a published
key, `signed_urls=verify` checks signatures in permissive mode as well, and the
identity and bucket documents can be written inline in `GCS_MIMIC_IDENTITIES`
and `GCS_MIMIC_BUCKETS`. The [strict local recipe](../../examples/strict-local/README.md)
combines them in a few settings. These are local extensions, not Cloud Storage
behavior; DESIGN-101 stays `PARTIAL` until a browser runs against the recipe.

## Missing capabilities, in priority order

Priority selects work when capacity is limited. Only an explicit prerequisite
blocks another track; a higher-priority row does not make every later row wait.

| Priority | User outcome | Prerequisite | Can run alongside |
| --- | --- | --- | --- |
| First | Complete safe bidirectional synchronization | Conflict/recovery controls, strict authorization and final platform evidence | Independent client/protocol work |
| Next | Verify migration and local adapters together | Quiesced source, explicit cutover and final SDK/container runs | Directory sync |
| Then | Close reference behavior and ordinary Rails setup | Scoped API/profile and credential contracts | Independent reference capabilities |
| Every release | Reconcile evidence and pass distribution gates | Platform, dependency and artifact verification | All implementation tracks |
| Stable contract | Release the frozen public contract as 1.0.0 | Contract review, release-data upgrades, candidate soak and final verification | Superset work continues in later minors |
| Superset goal | Claim the complete fake-gcs-server superset | Complete baseline and named extensions | Combine verified scopes |

Current work closes remaining acceptance cases in small increments.
The recorded migration checkpoint does not validate subsequent XML, ACL or
authentication changes; final combined verification remains an integration gate.
Continuous observation and guarded deletion are scoped
implementations, not merely future proposals. Findings/status paging, bounded
waits have native scoped checks; conflict/recovery controls, supported strict
startup and the combined final runs still require their own acceptance.
DESIGN-062, DESIGN-072 and DESIGN-074 remain `PARTIAL`.

The [sync contract](slices/directory-synchronization.md) and
[deletion controls](slices/external-deletion-controls.md) separate tested
revision-bound deletion from remaining conflict/control/recovery and platform
guarantees. A live CLI talks to the running server rather than opening its data
directory from another process.

Import is no longer an unimplemented track. The source-state and memory-snapshot
contracts add scoped class, metadata, names and history preservation. Remaining
limits include information already lost by filesystem normalization, quiescence,
cutover/rollback and platform boundaries. Seed `.metadata` files are literal
objects; durable import sidecars remain a separate explicitly selected format.
See the [import contract](slices/import-filesystem.md).

Since 0.8.0 there are scoped HTTP batch/method override, XML
list/copy/multipart, helper/reset/reseed routes, local event delivery, a Go
subprocess facade and bounded RAM storage. Scoped checks passed for the
checkpoint recorded in the combined evidence. Remaining work includes declared
JSON/XML fields,
the full exported Go API,
platform and provider-contract differences.
Existing legacy v1 RPCs still need full reference input/response coverage.
Notification configurations select immutable events into a bounded transactional
outbox; delivery requires a separately supplied local Pub/Sub emulator. This
does not implement Google's delivery service. HMAC keys now authenticate V4 XML
query and Authorization signatures through registered principals and current
key state. V4 HMAC and V2 RSA/HMAC form-policy cores now pass their declared
variant and write-failure subsets. Full form routing, fields, browser/client
and provider contracts remain pending. Legacy V2 HMAC queries and GOOG1/AWS
headers support the named native method and virtual-object subsets;
DESIGN-040 retains signed resumable, virtual bucket, provider and SDK boundaries.
AWS4 header-authenticated signed chunks now work for ordinary object PUT;
DESIGN-039 retains other operation/format and exact provider questions.
Autoclass automatic transitions and lifecycle action execution remain
absent; customer-supplied keys enforce reads without encrypting local disk bytes.
Broader Google behavior is separate from the pinned-reference minimum.
Source inventory determines the final reference set; these examples do not exclude
an unlisted source capability.

Current migration work follows the investigated fake-gcs-server workflow
obstacles and the approved responses in the [delivery plan](delivery-plan.md).
General Google API coverage is a separate objective; the
[feature inventory](feature-matrix.md) is not the task list or effort estimate
for this migration work. Its existing statuses and pending cases remain intact.

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

By default the bucket initialization file creates **missing buckets only**.
Restart keeps existing settings and objects, including API changes, and warns
when an edited entry was not applied. Removing a file entry does not delete a
bucket. Its [version-1 schema and local administrative authority](slices/bucket-initialization.md)
are implemented, including an `iamPolicy` a new bucket starts with. The opt-in
`buckets_file_mode=reconcile` applies the fields each entry writes to its
existing bucket at startup, except its location and owner. The same document
written inline in `GCS_MIMIC_BUCKETS` defaults to reconcile, because an edit
there is meant to apply at the next start; reconcile never resets a field that
an entry stops writing.

Directory sync must handle API/file conflicts, unusual names, partial writes,
missed events and recovery. 0.8.0 includes bounded conflict retention,
revision-bound resolution and guarded deletion. Both authentication profiles
support startup; strict controls require explicit project administrator grants
covering current buckets and retained original owners. Background filesystem
work uses separate startup administrative authority. Scoped native, SDK,
container and browser checks pass; remaining platform and broader workflow
acceptance stays explicit in the feature ledger. A permanent
write-back I/O error can delay graceful shutdown until space or permissions
are repaired.
SQLite remains the catalog; monitoring files and updating only their sizes is
not sufficient. See the [filesystem requirements](fake-gcs-server-storage.md#required-gcs-mimic-follow-up).

## Releases

Publish feature additions as successive `0.x` minor releases and compatible
fixes as patches. Do not wait for complete parity while staying at `0.1.0`.
Initializer, sync and import can ship separately; version numbers are assigned
to actual release scopes rather than reserved for tracks.

`1.0.0` freezes the public contract, upgrades data directories from every supported release (`0.8.0` and later) and follows a release-candidate soak.
The complete pinned-reference superset and full Google API coverage are separate goals.
See the [version policy and exact gate](release-policy.md#the-100-gate).
Releases `0.1.0` through `0.7.0` are [withdrawn](release-policy.md#withdrawn-releases).

## First useful release

The first useful `0.x` release has shipped; published examples now use `0.17.1`. The original gate covered scoped JSON, public/private access and URL workflows, persistent storage, fixtures, Go/Ruby, CORS and local HTTPS. Rails, lifecycle, gRPC and full parity were not prerequisites for that first release.

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
